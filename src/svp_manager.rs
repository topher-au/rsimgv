//! The local server SVP's libraries talk to, as svpctl runs it, on `/tmp/com.svpteam.svp`. SVP's flow library and control
//! plugin connect to it and exchange framed messages: a 7-byte header (magic, message type, parameter count, payload
//! length as a little-endian int32), then little-endian int32 parameters, then the payload.
//!
//! The server answers handshakes, and leaves SVP's settings as they are in its replies to per-frame reports. Clients are
//! served on threads of their own, so a busy main thread does not hold up the filter waiting on a reply.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Where SVP's flow library looks for the server.
const SOCKET_PATH: &str = "/tmp/com.svpteam.svp";

const MAGIC: u8 = 1;
const HEADER_SIZE: usize = 7;
/// A bigger payload means the stream has lost its place, as SVP sends nothing near this size.
const MAX_PAYLOAD: i32 = 64 * 1024 * 1024;
const READ_CHUNK: usize = 65536;

/// The only version of the protocol a handshake is accepted for.
const SUPPORTED_VERSION: i32 = 2;
/// A handshake's reply is the client's process id XORed with this.
const HANDSHAKE_KEY: i32 = 0x329120;
/// Sent in place of a value to leave the client's setting as it is.
const NO_CHANGE: i32 = -1000;

/// Message types from clients.
const HANDSHAKE: u8 = 0;
const FRAME_STAT: u8 = 1;
const PLUGIN_HANDSHAKE: u8 = 3;
const FRAME_ACK: u8 = 5;

/// Message types to clients.
const RATE_OVERRIDE: u8 = 1;
const HANDSHAKE_REPLY: u8 = 4;
const FLAG: u8 = 5;

/// A client's message, without its payload, which nothing here reads.
#[derive(Debug, PartialEq)]
struct Message {
    kind: u8,
    params: Vec<i32>,
}

/// The first message in `buffer` and the bytes it takes up, or `None` if it has not all arrived. An error means the
/// stream has lost its place, which cannot be recovered from.
fn next_message(buffer: &[u8]) -> Result<Option<(Message, usize)>, String> {
    let Some(header) = buffer.first_chunk::<HEADER_SIZE>() else { return Ok(None) };
    let [magic, kind, count, length @ ..] = *header;
    let length = i32::from_le_bytes(length);
    if magic != MAGIC {
        return Err(format!("bad magic byte {magic}"));
    }
    if !(0..=MAX_PAYLOAD).contains(&length) {
        return Err(format!("bad payload length {length}"));
    }
    let params_end = HEADER_SIZE + 4 * usize::from(count);
    let end = params_end + length as usize;
    if buffer.len() < end {
        return Ok(None);
    }
    let params = buffer[HEADER_SIZE..params_end]
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("chunks of 4 bytes")))
        .collect();
    Ok(Some((Message { kind, params }, end)))
}

/// A message to a client with `params` and no payload.
fn encode(kind: u8, params: &[i32]) -> Vec<u8> {
    let mut bytes = vec![MAGIC, kind, params.len() as u8, 0, 0, 0, 0];
    for param in params {
        bytes.extend_from_slice(&param.to_le_bytes());
    }
    bytes
}

/// The reply `message` is due, if any. A message the server cannot accept is reported and gets none.
fn reply_to(message: &Message) -> Option<Vec<u8>> {
    match (message.kind, message.params.as_slice()) {
        (HANDSHAKE, &[SUPPORTED_VERSION, process]) | (PLUGIN_HANDSHAKE, &[process]) => {
            println!("rsimgv: SVP manager: handshake from process {process}");
            Some(encode(HANDSHAKE_REPLY, &[process ^ HANDSHAKE_KEY]))
        }
        (FRAME_STAT, [_, _, _, _, _]) => Some(encode(RATE_OVERRIDE, &[NO_CHANGE, NO_CHANGE])),
        (FRAME_ACK, [_, _, _, _, _]) => Some(encode(FLAG, &[NO_CHANGE])),
        (HANDSHAKE | PLUGIN_HANDSHAKE | FRAME_STAT | FRAME_ACK, params) => {
            eprintln!("rsimgv: SVP manager: rejected message of type {} with parameters {params:?}", message.kind);
            None
        }
        _ => None,
    }
}

/// Answers `stream` until it closes or loses its place.
fn serve(mut stream: &UnixStream) -> Result<(), String> {
    let mut buffer = Vec::new();
    let mut chunk = vec![0; READ_CHUNK];
    loop {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        let mut consumed = 0;
        while let Some((message, size)) = next_message(&buffer[consumed..])? {
            consumed += size;
            if let Some(reply) = reply_to(&message) {
                stream.write_all(&reply).map_err(|err| err.to_string())?;
            }
        }
        buffer.drain(..consumed);
    }
}

/// Clears the way to listen at `path`: a socket left by a run that has ended is removed, but one that answers belongs to
/// a server that is running, such as svpctl or SVP's own, and is left alone.
fn claim(path: &Path) -> Result<(), String> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return Ok(()) };
    if !metadata.file_type().is_socket() {
        return Err(format!("{} is not a socket", path.display()));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(format!("another server is listening on {}", path.display()));
    }
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != ErrorKind::NotFound => Err(format!("could not remove {}: {err}", path.display())),
        _ => Ok(()),
    }
}

/// The running server. It stops, and removes its socket, when dropped.
pub struct Manager {
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    /// The connected clients, by the order they arrived, so they can be disconnected.
    clients: Arc<Mutex<HashMap<u64, UnixStream>>>,
    accepting: Option<JoinHandle<()>>,
}

impl Manager {
    fn start(path: &Path) -> Result<Self, String> {
        claim(path)?;
        let listener = UnixListener::bind(path).map_err(|err| format!("could not listen on {}: {err}", path.display()))?;
        if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            eprintln!("rsimgv: SVP manager: could not restrict {}: {err}", path.display());
        }
        println!("rsimgv: SVP manager listening on {}", path.display());
        let stopping = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(HashMap::new()));
        let accepting = {
            let (stopping, clients) = (stopping.clone(), clients.clone());
            std::thread::spawn(move || accept(&listener, &stopping, &clients))
        };
        Ok(Self { path: path.to_owned(), stopping, clients, accepting: Some(accepting) })
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Accepting blocks until a client arrives, so one is made for it to see that it should stop.
        if UnixStream::connect(&self.path).is_ok() {
            if let Some(accepting) = self.accepting.take() {
                let _ = accepting.join();
            }
        }
        for client in self.clients.lock().expect("the clients' lock").values() {
            let _ = client.shutdown(Shutdown::Both);
        }
        if let Err(err) = std::fs::remove_file(&self.path) {
            eprintln!("rsimgv: SVP manager: could not remove {}: {err}", self.path.display());
        }
        println!("rsimgv: SVP manager stopped");
    }
}

/// Takes clients from `listener` until `stopping` is set, serving each on a thread of its own.
fn accept(listener: &UnixListener, stopping: &AtomicBool, clients: &Arc<Mutex<HashMap<u64, UnixStream>>>) {
    let mut next_id = 0;
    for stream in listener.incoming() {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("rsimgv: SVP manager: could not accept a client: {err}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let id = next_id;
        next_id += 1;
        match stream.try_clone() {
            Ok(clone) => {
                clients.lock().expect("the clients' lock").insert(id, clone);
            }
            Err(err) => {
                eprintln!("rsimgv: SVP manager: could not keep a client: {err}");
                continue;
            }
        }
        let clients = clients.clone();
        std::thread::spawn(move || {
            if let Err(err) = serve(&stream) {
                eprintln!("rsimgv: SVP manager: client dropped: {err}");
            }
            clients.lock().expect("the clients' lock").remove(&id);
        });
    }
}

thread_local! {
    /// The server, while any viewer holds it, as every viewer in the process shares the one socket.
    static SHARED: RefCell<Weak<Manager>> = const { RefCell::new(Weak::new()) };
}

/// The server, started now if none is running.
pub fn shared() -> Result<Rc<Manager>, String> {
    if let Some(manager) = SHARED.with(|shared| shared.borrow().upgrade()) {
        return Ok(manager);
    }
    let manager = Rc::new(Manager::start(Path::new(SOCKET_PATH))?);
    SHARED.with(|shared| shared.replace(Rc::downgrade(&manager)));
    Ok(manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: u8, params: &[i32], payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![MAGIC, kind, params.len() as u8];
        bytes.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        for param in params {
            bytes.extend_from_slice(&param.to_le_bytes());
        }
        bytes.extend_from_slice(payload);
        bytes
    }

    fn socket_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rsimgv-svp-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn a_message_is_read_once_it_has_all_arrived() {
        let data = frame(HANDSHAKE, &[2, 99], b"payload");
        assert_eq!(next_message(&data[..3]), Ok(None));
        assert_eq!(next_message(&data[..10]), Ok(None));
        assert_eq!(next_message(&data), Ok(Some((Message { kind: HANDSHAKE, params: vec![2, 99] }, data.len()))));
    }

    #[test]
    fn a_stream_that_lost_its_place_is_an_error() {
        assert!(next_message(&[2, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(next_message(&[MAGIC, 0, 0, 0xfb, 0xff, 0xff, 0xff]).is_err());
        assert!(next_message(&frame(HANDSHAKE, &[], &[])[..6]).is_ok());
    }

    #[test]
    fn handshakes_are_answered_with_the_process_id_and_the_key() {
        let reply = encode(HANDSHAKE_REPLY, &[0x1234 ^ HANDSHAKE_KEY]);
        assert_eq!(reply_to(&Message { kind: HANDSHAKE, params: vec![2, 0x1234] }), Some(reply.clone()));
        assert_eq!(reply_to(&Message { kind: PLUGIN_HANDSHAKE, params: vec![0x1234] }), Some(reply));
    }

    #[test]
    fn other_versions_and_wrong_parameter_counts_get_no_reply() {
        assert_eq!(reply_to(&Message { kind: HANDSHAKE, params: vec![1, 0x1234] }), None);
        assert_eq!(reply_to(&Message { kind: HANDSHAKE, params: vec![2] }), None);
        assert_eq!(reply_to(&Message { kind: FRAME_STAT, params: vec![1, 2] }), None);
        assert_eq!(reply_to(&Message { kind: 7, params: vec![] }), None);
    }

    #[test]
    fn frame_reports_leave_the_settings_as_they_are() {
        let report = vec![7, 1619817716, 587, 578, 0];
        assert_eq!(reply_to(&Message { kind: FRAME_STAT, params: report.clone() }), Some(encode(RATE_OVERRIDE, &[NO_CHANGE, NO_CHANGE])));
        assert_eq!(reply_to(&Message { kind: FRAME_ACK, params: report }), Some(encode(FLAG, &[NO_CHANGE])));
    }

    #[test]
    fn replies_are_framed() {
        assert_eq!(encode(FLAG, &[1]), [MAGIC, FLAG, 1, 0, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn the_server_answers_over_its_socket_and_cleans_up() {
        let path = socket_path("answers");
        let manager = Manager::start(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(&frame(HANDSHAKE, &[2, 0x4242], &[])).unwrap();
        let expected = encode(HANDSHAKE_REPLY, &[0x4242 ^ HANDSHAKE_KEY]);
        let mut reply = vec![0; expected.len()];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, expected);

        drop(manager);
        assert!(!path.exists());
        assert_eq!(client.read(&mut reply).unwrap(), 0);
    }

    #[test]
    fn a_socket_left_behind_is_replaced_but_a_live_one_is_not() {
        let path = socket_path("claim");
        let live = UnixListener::bind(&path).unwrap();
        assert!(Manager::start(&path).is_err());
        assert!(path.exists());
        drop(live);

        // The listener is closed, and its file stays behind.
        let manager = Manager::start(&path).unwrap();
        drop(manager);
        assert!(!path.exists());
    }

    #[test]
    fn something_other_than_a_socket_is_left_alone() {
        let path = socket_path("file");
        std::fs::write(&path, "not a socket").unwrap();
        assert!(Manager::start(&path).is_err());
        assert!(path.is_file());
        std::fs::remove_file(&path).unwrap();
    }
}
