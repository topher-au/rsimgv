//! Reading and writing playlists in the standard formats: M3U (and M3U8), PLS and XSPF. Pure: no GTK.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// A playlist file format. `.m3u` and `.m3u8` are one format here, and both are written as UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    M3u,
    Pls,
    Xspf,
}

/// What a playlist file holds that can be used here: its local files, and how many entries were something else, such
/// as a web address.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub paths: Vec<PathBuf>,
    /// The title each path is given in the file, if it is. Parallel to `paths`.
    pub titles: Vec<Option<String>>,
    pub skipped: usize,
}

impl Parsed {
    fn push(&mut self, path: Option<PathBuf>, title: Option<String>) {
        match path {
            Some(path) => {
                self.paths.push(path);
                self.titles.push(title);
            }
            None => self.skipped += 1,
        }
    }
}

/// A title the file holds, if it holds one that is not empty.
fn nonempty(title: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

impl Format {
    /// The format a file's extension names, if it names one.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "m3u" | "m3u8" => Some(Self::M3u),
            "pls" => Some(Self::Pls),
            "xspf" => Some(Self::Xspf),
            _ => None,
        }
    }

    /// The format of the file text `text`, judged by how it starts, so a file with the wrong extension still reads.
    fn sniff(text: &str) -> Self {
        let start = text.trim_start();
        if start.get(..10).is_some_and(|head| head.eq_ignore_ascii_case("[playlist]")) {
            Self::Pls
        } else if start.starts_with('<') && start.contains("<playlist") {
            Self::Xspf
        } else {
            Self::M3u
        }
    }
}

/// The text of a playlist file holding `items`: absolute paths, each with the title it is shown by.
pub fn write(format: Format, items: &[(PathBuf, String)]) -> String {
    match format {
        Format::M3u => write_m3u(items),
        Format::Pls => write_pls(items),
        Format::Xspf => write_xspf(items),
    }
}

/// Reads the playlist file contents `bytes`, of any format. Relative entries are relative to `base`, the file's folder.
pub fn read(bytes: &[u8], base: &Path) -> Parsed {
    let text = decode(bytes);
    match Format::sniff(&text) {
        Format::M3u => read_m3u(&text, base),
        Format::Pls => read_pls(&text, base),
        Format::Xspf => read_xspf(&text, base),
    }
}

/// The text of a playlist file. Files that are not UTF-8 are older M3U and PLS files, taken as Latin-1.
pub(super) fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|byte| char::from(*byte)).collect(),
    }
}

fn write_m3u(items: &[(PathBuf, String)]) -> String {
    let mut text = String::from("#EXTM3U\n");
    for (item, title) in items {
        text.push_str(&format!("#EXTINF:-1,{}\n{}\n", one_line(title), plain_or_uri(item)));
    }
    text
}

fn read_m3u(text: &str, base: &Path) -> Parsed {
    let mut parsed = Parsed::default();
    // An #EXTINF line gives the title of the entry after it.
    let mut title = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(info) = trimmed.strip_prefix("#EXTINF:") {
            title = info.split_once(',').and_then(|(_, title)| nonempty(title));
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            parsed.push(entry_path(line, base, false), title.take());
        }
    }
    parsed
}

fn write_pls(items: &[(PathBuf, String)]) -> String {
    let mut text = String::from("[playlist]\n");
    for (index, (item, title)) in items.iter().enumerate() {
        let number = index + 1;
        text.push_str(&format!("File{number}={}\nTitle{number}={}\nLength{number}=-1\n", plain_or_uri(item), one_line(title)));
    }
    text.push_str(&format!("NumberOfEntries={}\nVersion=2\n", items.len()));
    text
}

fn read_pls(text: &str, base: &Path) -> Parsed {
    let mut files: Vec<(u64, &str)> = Vec::new();
    let mut titles: HashMap<u64, &str> = HashMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if let Some(number) = numbered(key, "file") {
            files.push((number, value));
        } else if let Some(number) = numbered(key, "title") {
            titles.insert(number, value);
        }
    }
    // The numbers give the order, whatever order the lines are in.
    files.sort_by_key(|(number, _)| *number);
    let mut parsed = Parsed::default();
    for (number, value) in files {
        parsed.push(entry_path(value, base, false), titles.get(&number).and_then(|title| nonempty(title)));
    }
    parsed
}

/// The number in a key such as `File3` or `Title3`, which is `name` followed by a number, whatever its case.
fn numbered(key: &str, name: &str) -> Option<u64> {
    let head = key.get(..name.len())?;
    let number = key.get(name.len()..)?;
    head.eq_ignore_ascii_case(name).then(|| number.parse().ok()).flatten()
}

fn write_xspf(items: &[(PathBuf, String)]) -> String {
    let mut text = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<playlist version=\"1\" xmlns=\"http://xspf.org/ns/0/\">\n  <trackList>\n");
    for (item, title) in items {
        text.push_str(&format!(
            "    <track>\n      <location>{}</location>\n      <title>{}</title>\n    </track>\n",
            xml_escape(&file_uri(item)),
            xml_escape(&one_line(title))
        ));
    }
    text.push_str("  </trackList>\n</playlist>\n");
    text
}

/// A track's first location is the one used; any others are other places to find the same thing.
fn read_xspf(text: &str, base: &Path) -> Parsed {
    let text = without_xml_comments(text);
    let mut parsed = Parsed::default();
    for track in xml_elements(&text, "track") {
        if let Some(location) = xml_elements(track, "location").first() {
            let title = xml_elements(track, "title").first().and_then(|title| nonempty(&xml_text(title)));
            parsed.push(entry_path(xml_text(location).trim(), base, true), title);
        }
    }
    parsed
}

/// The name a player shows for `path` when it has no other: its file name without the extension, on one line.
pub fn default_title(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or(path.as_os_str()).to_string_lossy();
    one_line(&stem)
}

/// `text` on one line, as a title must be in a line-based file.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// `path` as a line of text: as it is, or as a URI if it would not survive that, not being UTF-8 or holding a line break.
fn plain_or_uri(path: &Path) -> String {
    match path.to_str() {
        Some(text) if !text.contains(['\n', '\r']) => text.to_owned(),
        _ => file_uri(path),
    }
}

/// The `file://` URI of the absolute path `path`.
pub(super) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => uri.push(char::from(*byte)),
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The local path an entry names, or `None` for an entry that is not a local file. A relative entry is taken from
/// `base`, percent-encoded if `encoded`, as XSPF's are.
fn entry_path(entry: &str, base: &Path, encoded: bool) -> Option<PathBuf> {
    match uri_scheme(entry) {
        Some(scheme) if scheme.eq_ignore_ascii_case("file") => file_uri_path(&entry[scheme.len() + 1..]),
        Some(_) => None,
        None => {
            let decoded;
            let relative = if encoded {
                decoded = percent_decode(entry);
                decoded.as_slice()
            } else {
                entry.as_bytes()
            };
            Some(normalized(&base.join(OsStr::from_bytes(relative))))
        }
    }
}

/// The scheme of `entry`, if it is a URI: a word before `://`, or `file:`. Anything else with a colon is a file name.
fn uri_scheme(entry: &str) -> Option<&str> {
    let (scheme, rest) = entry.split_once(':')?;
    let is_scheme = scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (is_scheme && (rest.starts_with("//") || scheme.eq_ignore_ascii_case("file"))).then_some(scheme)
}

/// The path of a `file:` URI, given what follows the `file:`. One on another computer is not a local file.
fn file_uri_path(rest: &str) -> Option<PathBuf> {
    let path = match rest.strip_prefix("//") {
        Some(rest) => {
            let (host, path) = rest.split_at(rest.find('/')?);
            if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            path
        }
        None => rest,
    };
    Some(normalized(Path::new(OsStr::from_bytes(&percent_decode(path)))))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = (bytes[index] == b'%').then(|| text.get(index + 1..index + 3)).flatten().and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    decoded
}

/// `path` with its `.` and `..` worked out, so it is the same path the browse view lists the file under.
fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// The text an element's content stands for: its CDATA as it is, and otherwise with its character references worked out.
fn xml_text(content: &str) -> String {
    if let Some(cdata) = content.trim().strip_prefix("<![CDATA[").and_then(|rest| rest.strip_suffix("]]>")) {
        return cdata.to_owned();
    }
    let mut text = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find('&') {
        text.push_str(&rest[..start]);
        rest = &rest[start..];
        let reference = rest.find(';').and_then(|end| Some((xml_reference(&rest[1..end])?, end)));
        match reference {
            Some((c, end)) => {
                text.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                text.push('&');
                rest = &rest[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// The character the XML reference `name` (what is between its `&` and `;`) stands for.
fn xml_reference(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

fn without_xml_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        kept.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map_or("", |end| &rest[start + end + 3..]);
    }
    kept.push_str(rest);
    kept
}

/// The contents of each element called `name` in `text`, in order. Elements of the same name inside each other are not
/// looked for, as XSPF has none.
fn xml_elements<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        // `<track` also starts `<trackList`.
        if !after.starts_with(|c: char| c == '>' || c == '/' || c.is_whitespace()) {
            rest = after;
            continue;
        }
        let Some(tag_end) = after.find('>') else { break };
        let body = &after[tag_end + 1..];
        if after[..tag_end].ends_with('/') {
            rest = body;
            continue;
        }
        let Some(end) = body.find(&close) else { break };
        found.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn awkward_items() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/photos/plain.png"),
            PathBuf::from("/photos/with space & <angle> 'quote' 100%.mp4"),
            PathBuf::from("/photos/ünï çødé/日本語.webm"),
            PathBuf::from("/photos/#hash.png"),
            PathBuf::from("/photos/line\nbreak.png"),
            PathBuf::from(OsString::from_vec(b"/photos/not-utf8-\xff.png".to_vec())),
        ]
    }

    /// The awkward paths, each with a title that needs care in a file too.
    fn titled_items() -> Vec<(PathBuf, String)> {
        let titles = ["Plain", "With, a comma & <angle>", "Ünï title", "Hash", "Two\nlines", "Not UTF-8"];
        awkward_items().into_iter().zip(titles).map(|(path, title)| (path, title.to_owned())).collect()
    }

    #[test]
    fn every_format_round_trips_awkward_paths_and_titles() {
        let expected_titles = ["Plain", "With, a comma & <angle>", "Ünï title", "Hash", "Two lines", "Not UTF-8"];
        for format in [Format::M3u, Format::Pls, Format::Xspf] {
            let text = write(format, &titled_items());
            let parsed = read(text.as_bytes(), Path::new("/elsewhere"));
            let expected = Parsed {
                paths: awkward_items(),
                titles: expected_titles.map(|title| Some(title.to_owned())).to_vec(),
                skipped: 0,
            };
            assert_eq!(parsed, expected, "{format:?}: {text}");
        }
    }

    #[test]
    fn an_empty_playlist_round_trips() {
        for format in [Format::M3u, Format::Pls, Format::Xspf] {
            assert_eq!(read(write(format, &[]).as_bytes(), Path::new("/")), Parsed::default(), "{format:?}");
        }
    }

    #[test]
    fn an_entry_without_a_title_has_none() {
        let text = "#EXTM3U\n/a.png\n#EXTINF:-1,\n/b.png\n";
        let parsed = read(text.as_bytes(), Path::new("/"));
        assert_eq!(parsed.titles, [None, None]);
    }

    #[test]
    fn the_extension_names_the_format() {
        assert_eq!(Format::from_path(Path::new("a.m3u")), Some(Format::M3u));
        assert_eq!(Format::from_path(Path::new("a.M3U8")), Some(Format::M3u));
        assert_eq!(Format::from_path(Path::new("a.pls")), Some(Format::Pls));
        assert_eq!(Format::from_path(Path::new("a.xspf")), Some(Format::Xspf));
        assert_eq!(Format::from_path(Path::new("a.txt")), None);
        assert_eq!(Format::from_path(Path::new("m3u")), None);
    }

    #[test]
    fn m3u_entries_can_be_relative_or_uris_and_web_addresses_are_skipped() {
        let text = "\u{feff}#EXTM3U\r\n#EXTINF:12,Title\r\nclip.mp4\r\n\r\n../up.png\r\n./sub/x.png\r\n/abs/y.png\r\nfile:///abs/z%20z.png\r\nfile://localhost/abs/l.png\r\nfile://other/abs/r.png\r\nhttps://example.com/a.mp4\r\nodd:name.png\r\n";
        let parsed = read(text.as_bytes(), Path::new("/lists/mine"));
        assert_eq!(
            parsed.paths,
            ["/lists/mine/clip.mp4", "/lists/up.png", "/lists/mine/sub/x.png", "/abs/y.png", "/abs/z z.png", "/abs/l.png", "/lists/mine/odd:name.png"]
                .map(PathBuf::from)
        );
        assert_eq!(parsed.skipped, 2);
    }

    #[test]
    fn an_older_m3u_in_latin1_still_reads() {
        let parsed = read(b"/photos/caf\xe9.png\n", Path::new("/"));
        assert_eq!(parsed.paths, [PathBuf::from("/photos/café.png")]);
    }

    #[test]
    fn pls_entries_follow_their_numbers() {
        let text = "[Playlist]\nNumberOfEntries=3\nfile2=/b.png\nTitle2=B\nFile10=/c.png\nFile1=/a.png\nFileX=/no.png\nVersion=2\n";
        assert_eq!(read(text.as_bytes(), Path::new("/")).paths, ["/a.png", "/b.png", "/c.png"].map(PathBuf::from));
    }

    #[test]
    fn xspf_takes_each_tracks_first_location_and_only_those() {
        let text = r#"<?xml version="1.0"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <location>http://example.com/this-playlist.xspf</location>
  <!-- <track><location>/commented/out.png</location></track> -->
  <trackList>
    <track><title>One</title><location>file:///a%20b.png</location><location>file:///alternate.png</location></track>
    <track><location>rel%20ative/c&amp;d.png</location></track>
    <track><location><![CDATA[file:///cdata.png]]></location></track>
    <track><location>&#47;numeric&#x2F;e.png</location></track>
    <track/>
    <track><title>No location</title></track>
    <track><location>http://example.com/web.mp4</location></track>
  </trackList>
</playlist>
"#;
        let parsed = read(text.as_bytes(), Path::new("/lists"));
        assert_eq!(parsed.paths, ["/a b.png", "/lists/rel ative/c&d.png", "/cdata.png", "/numeric/e.png"].map(PathBuf::from));
        assert_eq!(parsed.skipped, 1);
    }

    #[test]
    fn broken_escapes_are_kept_as_they_are() {
        assert_eq!(percent_decode("100%.png"), b"100%.png");
        assert_eq!(percent_decode("a%zzb%2"), b"a%zzb%2");
        assert_eq!(xml_text("a & b &bogus; &amp;"), "a & b &bogus; &");
    }

    #[test]
    fn the_file_decides_its_format_whatever_it_is_called() {
        assert_eq!(Format::sniff("  [playlist]\nFile1=/a\n"), Format::Pls);
        assert_eq!(Format::sniff("<?xml?><playlist/>"), Format::Xspf);
        assert_eq!(Format::sniff("#EXTM3U\n/a\n"), Format::M3u);
        assert_eq!(Format::sniff("/a\n"), Format::M3u);
    }
}
