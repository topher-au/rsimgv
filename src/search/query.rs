//! A search query: an ordered list of tokens, all of which an item must match.
//!
//! The text is read as three kinds of token. A known key followed by an operator is a comparison, such as `size<10`,
//! `duration>1:30` or `modified>=2024-01-31`. Text in double quotes is a phrase, which matches a full value exactly.
//! Anything else is a keyword, which matches a part of the name. Text that does not parse, such as `banana<10`, is a
//! keyword too, and an unclosed quote is plain text.

use std::fmt;

/// The keys a comparison can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Name,
    Size,
    Modified,
    Type,
    Width,
    Height,
    Fps,
    Duration,
}

/// What a key's value is, which decides how it is read and which operators it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Compared with `=` only, ignoring case.
    Text,
    /// A number, compared with any operator.
    Number,
    /// A day, compared with any operator.
    Date,
    /// A length of time in seconds, compared with any operator.
    Timespan,
}

impl Key {
    pub const ALL: [Key; 8] = [
        Key::Name,
        Key::Size,
        Key::Modified,
        Key::Type,
        Key::Width,
        Key::Height,
        Key::Fps,
        Key::Duration,
    ];

    /// The key's name as it is typed.
    pub fn name(self) -> &'static str {
        match self {
            Key::Name => "name",
            Key::Size => "size",
            Key::Modified => "modified",
            Key::Type => "type",
            Key::Width => "width",
            Key::Height => "height",
            Key::Fps => "fps",
            Key::Duration => "duration",
        }
    }

    /// The key with the name `text`, ignoring case.
    pub fn from_name(text: &str) -> Option<Key> {
        let lower = text.to_lowercase();
        Key::ALL.into_iter().find(|key| key.name() == lower)
    }

    fn kind(self) -> Kind {
        match self {
            Key::Name | Key::Type => Kind::Text,
            Key::Size | Key::Width | Key::Height | Key::Fps => Kind::Number,
            Key::Modified => Kind::Date,
            Key::Duration => Kind::Timespan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Less,
    LessEq,
    Greater,
    GreaterEq,
    Equal,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Less => "<",
            Op::LessEq => "<=",
            Op::Greater => ">",
            Op::GreaterEq => ">=",
            Op::Equal => "=",
        }
    }
}

/// The value of a comparison, read for its key's kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// Lower case, as the comparison matches ignoring case.
    Text(String),
    /// A number of bytes, or a plain number.
    Number(f64),
    /// A length of time in seconds.
    Seconds(f64),
    /// One local day, from its first second up to, not including, the first second of the next day. In Unix seconds.
    Day { start: i64, end: i64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A key compared with a value. `raw` is the value as typed. `operand` is `None` when the value does not read as the
    /// key's kind, in which case nothing matches.
    Cmp {
        key: Key,
        op: Op,
        raw: String,
        operand: Option<Operand>,
    },
    /// Text between quotes, which must equal a value exactly.
    Phrase(String),
    /// A keyword, which must be part of the name.
    Text(String),
}

/// Reads the text the user typed as a list of tokens, in the order they were typed.
pub fn parse(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return tokens;
        }
        if let Some((phrase, used)) = read_phrase(rest) {
            tokens.push(Token::Phrase(phrase));
            rest = &rest[used..];
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        tokens.push(word_token(&rest[..end]));
        rest = &rest[end..];
    }
}

/// Reads a phrase that starts `text`, returning its text and how many bytes of `text` it takes. `\"` is a quote in the
/// phrase. `None` if the phrase is not closed, so the quote is read as plain text.
fn read_phrase(text: &str) -> Option<(String, usize)> {
    let body = text.strip_prefix('"')?;
    let mut phrase = String::new();
    let mut chars = body.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((phrase, 1 + index + 1)),
            '\\' if body[index + 1..].starts_with('"') => {
                phrase.push('"');
                chars.next();
            }
            _ => phrase.push(c),
        }
    }
    None
}

/// The token for one word that is not a phrase: a comparison if it reads as one, otherwise a keyword.
fn word_token(word: &str) -> Token {
    let keyword = || Token::Text(word.to_owned());
    let Some(split) = word.find(['<', '>', '=']) else { return keyword() };
    let Some(key) = Key::from_name(&word[..split]) else { return keyword() };
    let after = &word[split..];
    let (op, length) = if after.starts_with("<=") {
        (Op::LessEq, 2)
    } else if after.starts_with(">=") {
        (Op::GreaterEq, 2)
    } else if after.starts_with('<') {
        (Op::Less, 1)
    } else if after.starts_with('>') {
        (Op::Greater, 1)
    } else {
        (Op::Equal, 1)
    };
    let raw = &after[length..];
    // A key with no value yet is text until the value is typed, and text keys only take `=`.
    if raw.is_empty() || (key.kind() == Kind::Text && op != Op::Equal) {
        return keyword();
    }
    Token::Cmp { key, op, raw: raw.to_owned(), operand: parse_operand(key, raw) }
}

fn parse_operand(key: Key, raw: &str) -> Option<Operand> {
    match key.kind() {
        Kind::Text => Some(Operand::Text(raw.to_lowercase())),
        // Sizes take units, and other numbers are plain.
        Kind::Number => parse_number(raw, key == Key::Size).map(Operand::Number),
        Kind::Timespan => parse_timespan(raw).map(Operand::Seconds),
        Kind::Date => parse_day(raw).map(|(start, end)| Operand::Day { start, end }),
    }
}

/// A non-negative number, optionally with a decimal unit suffix: `K`, `M`, `G` or `T` for 1000, 1000², 1000³ or 1000⁴.
fn parse_number(raw: &str, with_units: bool) -> Option<f64> {
    let (digits, scale) = match raw.char_indices().last() {
        Some((index, unit)) if with_units => match unit.to_ascii_uppercase() {
            'K' => (&raw[..index], 1e3),
            'M' => (&raw[..index], 1e6),
            'G' => (&raw[..index], 1e9),
            'T' => (&raw[..index], 1e12),
            _ => (raw, 1.0),
        },
        _ => (raw, 1.0),
    };
    let value: f64 = digits.parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value * scale)
}

/// A length of time: `90s`, `1:30` or `1:02:03`. Only the seconds part may have a fraction.
fn parse_timespan(raw: &str) -> Option<f64> {
    if let Some(seconds) = raw.strip_suffix('s') {
        return unsigned(seconds, true);
    }
    let parts: Vec<&str> = raw.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total = 0.0;
    for (index, part) in parts.iter().enumerate() {
        let is_seconds = index + 1 == parts.len();
        total = total * 60.0 + unsigned(part, is_seconds)?;
    }
    Some(total)
}

/// A number written only as digits, with one decimal point if `fraction` allows it.
fn unsigned(text: &str, fraction: bool) -> Option<f64> {
    let dots = text.matches('.').count();
    let valid = !text.is_empty() && dots <= usize::from(fraction) && text.chars().all(|c| c.is_ascii_digit() || c == '.');
    valid.then(|| text.parse().ok()).flatten()
}

/// A day written `YYYY-MM-DD`, as the seconds where it starts and where the next day starts.
fn parse_day(raw: &str) -> Option<(i64, i64)> {
    let mut parts = raw.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: i32 = parts.next()?.parse().ok()?;
    let day: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let start = gtk::glib::DateTime::from_local(year, month, day, 0, 0, 0.0).ok()?;
    let end = start.add_days(1).ok()?;
    Some((start.to_unix(), end.to_unix()))
}

impl fmt::Display for Token {
    /// The token as it is typed, which `parse` reads back as the same token.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Cmp { key, op, raw, .. } => write!(f, "{}{}{}", key.name(), op.symbol(), raw),
            Token::Phrase(text) => write!(f, "\"{}\"", text.replace('"', "\\\"")),
            Token::Text(text) => f.write_str(text),
        }
    }
}

/// The query as text, from its tokens. `parse` reads it back as the same tokens.
pub fn serialize(tokens: &[Token]) -> String {
    tokens.iter().map(Token::to_string).collect::<Vec<_>>().join(" ")
}

/// What an item says about itself, as the matcher reads it. Metadata fields are `None` until they are known.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub name: String,
    pub content_type: String,
    /// In bytes.
    pub size: u64,
    /// In Unix seconds.
    pub modified: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// In frames a second.
    pub fps: Option<f64>,
    /// In seconds.
    pub duration: Option<f64>,
}

impl Token {
    pub fn matches(&self, facts: &Facts) -> bool {
        match self {
            Token::Phrase(text) => {
                facts.name.to_lowercase() == text.to_lowercase() || facts.content_type.to_lowercase() == text.to_lowercase()
            }
            Token::Text(text) => facts.name.to_lowercase().contains(&text.to_lowercase()),
            Token::Cmp { key, op, operand: Some(operand), .. } => compare(*key, *op, operand, facts),
            Token::Cmp { operand: None, .. } => false,
        }
    }

    /// Whether the token needs a metadata field that may not be known yet.
    pub fn needs_metadata(&self) -> bool {
        matches!(self, Token::Cmp { key: Key::Width | Key::Height | Key::Fps | Key::Duration, .. })
    }
}

/// Whether an item with `facts` matches every token. An empty query matches everything.
pub fn matches_all(tokens: &[Token], facts: &Facts) -> bool {
    tokens.iter().all(|token| token.matches(facts))
}

fn compare(key: Key, op: Op, operand: &Operand, facts: &Facts) -> bool {
    match (key, operand) {
        (Key::Name, Operand::Text(text)) => facts.name.to_lowercase() == *text,
        (Key::Type, Operand::Text(text)) => facts.content_type.to_lowercase() == *text,
        (Key::Size, Operand::Number(value)) => order(op, facts.size as f64, *value),
        (Key::Width, Operand::Number(value)) => facts.width.is_some_and(|width| order(op, f64::from(width), *value)),
        (Key::Height, Operand::Number(value)) => facts.height.is_some_and(|height| order(op, f64::from(height), *value)),
        // Frame rates are compared to two decimal places, as they are shown, so that 29.97 matches 30000/1001.
        (Key::Fps, Operand::Number(value)) => facts.fps.is_some_and(|fps| {
            let (fps, value) = ((fps * 100.0).round(), (value * 100.0).round());
            order(op, fps, value)
        }),
        (Key::Duration, Operand::Seconds(seconds)) => facts.duration.is_some_and(|duration| order(op, duration, *seconds)),
        (Key::Modified, Operand::Day { start, end }) => {
            let modified = facts.modified;
            match op {
                Op::Less => modified < *start,
                Op::LessEq => modified < *end,
                Op::Greater => modified >= *end,
                Op::GreaterEq => modified >= *start,
                Op::Equal => (*start..*end).contains(&modified),
            }
        }
        _ => false,
    }
}

fn order(op: Op, actual: f64, expected: f64) -> bool {
    match op {
        Op::Less => actual < expected,
        Op::LessEq => actual <= expected,
        Op::Greater => actual > expected,
        Op::GreaterEq => actual >= expected,
        Op::Equal => actual == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(key: Key, op: Op, raw: &str, operand: Option<Operand>) -> Token {
        Token::Cmp { key, op, raw: raw.into(), operand }
    }

    fn photo() -> Facts {
        Facts {
            name: "Holiday.JPG".into(),
            content_type: "image/jpeg".into(),
            size: 2_500_000,
            modified: 1_706_659_200,
            width: Some(4000),
            height: Some(3000),
            fps: None,
            duration: None,
        }
    }

    #[test]
    fn a_known_key_and_operator_is_a_comparison() {
        assert_eq!(parse("size<10"), vec![cmp(Key::Size, Op::Less, "10", Some(Operand::Number(10.0)))]);
        assert_eq!(parse("size=10")[0], cmp(Key::Size, Op::Equal, "10", Some(Operand::Number(10.0))));
        assert!(matches!(parse("width>=1920")[0], Token::Cmp { key: Key::Width, op: Op::GreaterEq, .. }));
    }

    #[test]
    fn keys_and_units_are_read_regardless_of_case_for_keys() {
        assert_eq!(parse("SIZE<=2m")[0], cmp(Key::Size, Op::LessEq, "2m", Some(Operand::Number(2e6))));
        assert_eq!(parse("size<1.5K")[0], cmp(Key::Size, Op::Less, "1.5K", Some(Operand::Number(1500.0))));
    }

    #[test]
    fn an_unknown_key_stays_text() {
        assert_eq!(parse("banana<10"), vec![Token::Text("banana<10".into())]);
    }

    #[test]
    fn a_key_without_a_value_is_text_until_one_is_typed() {
        assert_eq!(parse("size<"), vec![Token::Text("size<".into())]);
    }

    #[test]
    fn text_keys_only_take_equality() {
        assert_eq!(parse("name<5"), vec![Token::Text("name<5".into())]);
        assert!(matches!(parse("name=Holiday")[0], Token::Cmp { key: Key::Name, op: Op::Equal, .. }));
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_an_invalid_comparison() {
        assert_eq!(parse("size<ten"), vec![cmp(Key::Size, Op::Less, "ten", None)]);
        assert!(!parse("size<ten")[0].matches(&photo()));
    }

    #[test]
    fn quoted_text_is_a_phrase_and_its_contents_are_literal() {
        assert_eq!(parse("\"size<10\""), vec![Token::Phrase("size<10".into())]);
        assert_eq!(parse("\"this is a string\""), vec![Token::Phrase("this is a string".into())]);
        assert_eq!(parse("\"a \\\"b\\\" c\""), vec![Token::Phrase("a \"b\" c".into())]);
    }

    #[test]
    fn an_unclosed_quote_is_plain_text() {
        assert_eq!(parse("\"unclosed text"), vec![Token::Text("\"unclosed".into()), Token::Text("text".into())]);
    }

    #[test]
    fn words_are_separated_by_spaces_and_tokens_keep_their_order() {
        let tokens = parse("  holiday  size>1M \"my file\" ");
        assert_eq!(
            tokens,
            vec![
                Token::Text("holiday".into()),
                cmp(Key::Size, Op::Greater, "1M", Some(Operand::Number(1e6))),
                Token::Phrase("my file".into()),
            ]
        );
    }

    #[test]
    fn durations_read_as_seconds_in_either_form() {
        assert_eq!(parse("duration<90s")[0], cmp(Key::Duration, Op::Less, "90s", Some(Operand::Seconds(90.0))));
        assert_eq!(parse("duration>1:30")[0], cmp(Key::Duration, Op::Greater, "1:30", Some(Operand::Seconds(90.0))));
        assert_eq!(parse("duration>1:02:03")[0], cmp(Key::Duration, Op::Greater, "1:02:03", Some(Operand::Seconds(3723.0))));
        assert_eq!(parse("duration>90")[0], cmp(Key::Duration, Op::Greater, "90", None), "a bare number is not a time");
    }

    #[test]
    fn a_day_reads_as_the_seconds_it_covers() {
        let Token::Cmp { operand: Some(Operand::Day { start, end }), .. } = parse("modified>=2024-01-31")[0].clone() else {
            panic!("a day");
        };
        assert_eq!(end - start, 86_400);
        assert!(!parse("modified=2024-02-30")[0].matches(&photo()), "not a day of the calendar");
    }

    #[test]
    fn serialising_gives_back_the_same_tokens() {
        for text in ["size<=1.5K banana<10 \"a \\\"b\\\"\" holiday", "duration>1:30 modified>=2024-01-31", "size<"] {
            let tokens = parse(text);
            assert_eq!(parse(&serialize(&tokens)), tokens, "{text}");
        }
    }

    #[test]
    fn size_compares_exact_bytes() {
        assert!(parse("size=2500000")[0].matches(&photo()));
        assert!(!parse("size=2500001")[0].matches(&photo()));
        assert!(parse("size>=2.5M")[0].matches(&photo()));
    }

    #[test]
    fn text_keys_match_whole_values_ignoring_case() {
        assert!(parse("name=holiday.jpg")[0].matches(&photo()));
        assert!(!parse("name=holiday")[0].matches(&photo()));
        assert!(parse("type=IMAGE/JPEG")[0].matches(&photo()));
    }

    #[test]
    fn a_keyword_matches_part_of_the_name() {
        assert!(Token::Text("hol".into()).matches(&photo()));
        assert!(!Token::Text("summer".into()).matches(&photo()));
    }

    #[test]
    fn a_phrase_must_equal_a_value_exactly() {
        assert!(Token::Phrase("holiday.jpg".into()).matches(&photo()));
        assert!(!Token::Phrase("holiday".into()).matches(&photo()));
    }

    #[test]
    fn unknown_metadata_never_matches() {
        let width = parse("width>1000")[0].clone();
        assert!(width.matches(&photo()));
        assert!(!width.matches(&Facts { width: None, ..photo() }));
        assert!(width.needs_metadata());
        assert!(!parse("size>1")[0].needs_metadata());
    }

    #[test]
    fn frame_rates_compare_to_two_decimal_places() {
        let video = Facts { fps: Some(30000.0 / 1001.0), ..photo() };
        assert!(parse("fps=29.97")[0].matches(&video));
        assert!(parse("fps>29")[0].matches(&video));
    }

    #[test]
    fn a_day_compares_the_whole_day() {
        // Days are local, so the start of the day is taken from the parser rather than assumed.
        let Token::Cmp { operand: Some(Operand::Day { start, .. }), .. } = parse("modified=2024-01-31")[0].clone() else {
            panic!("a day");
        };
        let modified = Facts { modified: start + 3_600, ..photo() };
        assert!(parse("modified=2024-01-31")[0].matches(&modified));
        assert!(parse("modified>=2024-01-31")[0].matches(&modified));
        assert!(!parse("modified>2024-01-31")[0].matches(&modified));
        assert!(!parse("modified<2024-01-31")[0].matches(&modified));
    }

    #[test]
    fn every_token_must_match() {
        let tokens = parse("holiday width>=4000 size<3M");
        assert!(matches_all(&tokens, &photo()));
        assert!(!matches_all(&parse("holiday width>5000"), &photo()));
        assert!(matches_all(&[], &photo()), "an empty query matches everything");
    }
}
