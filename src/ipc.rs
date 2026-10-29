//! The IPC: JSON requests that drive a running rsimgv, over a Unix socket, a TCP socket, or HTTP.
//!
//! A request is `{"id": 1, "method": "tabs.list", "params": {}}`, and its answer is either
//! `{"id": 1, "result": {...}}` or `{"id": 1, "error": {"code": "...", "message": "..."}}`.
//!
//! - The Unix socket and the TCP socket take one request per line, and answer with one line per request.
//!   The TCP socket listens on 127.0.0.1 only, unless external access is on, when it listens on all interfaces.
//! - HTTP takes a request as the body of `POST /ipc`, sent with `Content-Type: application/json`, and answers
//!   with the response as the body. A request without that content type is refused: a web page cannot send
//!   it cross-site without a preflight request, which is never answered.
//!
//! The socket is `rsimgv.sock` in the user's runtime directory unless a path is set, with mode 0600. Methods are in `remote`.
//! Requests are carried out on the GTK thread, where the window is.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gtk::glib;
use serde_json::{json, Value};

/// How long a transport waits for the GTK thread to answer before it gives up.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest HTTP body accepted, in bytes.
const MAX_BODY: usize = 1 << 20;
/// How often an idle listener checks whether it has been told to stop.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// A request for the GTK thread to carry out. Its answer goes back to the transport that sent it.
pub struct Call {
    pub method: String,
    pub params: Value,
    reply: mpsc::Sender<Result<Value, Error>>,
}

impl Call {
    pub fn respond(self, answer: Result<Value, Error>) {
        // The transport may have timed out and gone; there is nobody left to tell.
        let _ = self.reply.send(answer);
    }
}

/// A failed request: a short code a client can match on, and a sentence for people.
#[derive(Debug)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// Which transports to serve. `socket` is the Unix socket's path, and `None` leaves it off. A TCP or HTTP port of 0
/// leaves that transport off. With `external`, the TCP and HTTP ports listen on all interfaces rather than on localhost.
pub struct Config {
    pub socket: Option<PathBuf>,
    pub tcp_port: u16,
    pub http_port: u16,
    pub external: bool,
}

/// Where the Unix socket is when no path is set: `rsimgv.sock` in the user's runtime directory.
pub fn default_socket_path() -> PathBuf {
    glib::user_runtime_dir().join("rsimgv.sock")
}

/// One running transport: the thread accepting its connections, and what to clean up when it stops.
struct Listener {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    socket: Option<PathBuf>,
}

/// The running transports. `apply` stops them all and starts the ones a config enables, so a settings change takes
/// effect at once. Calls from every transport arrive on the channel the server was made with.
pub struct Server {
    calls: mpsc::Sender<Call>,
    listeners: Vec<Listener>,
}

impl Server {
    pub fn new(calls: mpsc::Sender<Call>) -> Self {
        Self { calls, listeners: Vec::new() }
    }

    /// Stops the running transports, then starts each one `config` enables. A transport that cannot start is
    /// reported, and the others still run.
    pub fn apply(&mut self, config: &Config) {
        self.stop();
        if let Some(path) = &config.socket {
            let calls = self.calls.clone();
            match serve_unix(path, calls) {
                Ok(listener) => self.listeners.push(listener),
                Err(err) => eprintln!("rsimgv: could not open the IPC socket {}: {err}", path.display()),
            }
        }
        let host = if config.external { "0.0.0.0" } else { "127.0.0.1" };
        if config.tcp_port != 0 {
            let calls = self.calls.clone();
            match serve_tcp(host, config.tcp_port, move |stream| {
                let _ = serve_lines(stream, &calls);
            }) {
                Ok(listener) => self.listeners.push(listener),
                Err(err) => eprintln!("rsimgv: could not open the IPC TCP port {}: {err}", config.tcp_port),
            }
        }
        if config.http_port != 0 {
            let calls = self.calls.clone();
            match serve_tcp(host, config.http_port, move |stream| {
                let _ = serve_http_request(stream, &calls);
            }) {
                Ok(listener) => self.listeners.push(listener),
                Err(err) => eprintln!("rsimgv: could not open the IPC HTTP port {}: {err}", config.http_port),
            }
        }
    }

    /// Stops every running transport. Each one has finished accepting when this returns; connections already
    /// being answered finish on their own.
    pub fn stop(&mut self) {
        for listener in self.listeners.drain(..) {
            listener.stop.store(true, Ordering::SeqCst);
            let _ = listener.thread.join();
            if let Some(path) = listener.socket {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Accepts connections until `stop` is set, handing each one to `serve` on its own thread.
fn accept_loop<S: Send + 'static>(
    stop: Arc<AtomicBool>,
    mut accept: impl FnMut() -> io::Result<S> + Send + 'static,
    serve: impl Fn(S) + Clone + Send + 'static,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match accept() {
                Ok(stream) => {
                    let serve = serve.clone();
                    thread::spawn(move || serve(stream));
                }
                // Nothing is waiting (or accept failed); look again after a moment, so a stop is noticed.
                Err(_) => thread::sleep(ACCEPT_POLL),
            }
        }
    })
}

fn serve_unix(path: &Path, calls: mpsc::Sender<Call>) -> io::Result<Listener> {
    // A socket left by an earlier run would stop the bind, so it goes first.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = accept_loop(
        stop.clone(),
        move || listener.accept().map(|(stream, _)| stream),
        move |stream: UnixStream| {
            let _ = serve_lines(stream, &calls);
        },
    );
    Ok(Listener { stop, thread, socket: Some(path.to_owned()) })
}

/// Listens on `port` on `host`, handing each connection to `serve`.
fn serve_tcp(host: &str, port: u16, serve: impl Fn(TcpStream) + Clone + Send + 'static) -> io::Result<Listener> {
    let listener = TcpListener::bind((host, port))?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = accept_loop(stop.clone(), move || listener.accept().map(|(stream, _)| stream), serve);
    Ok(Listener { stop, thread, socket: None })
}

/// Serves one connection of the line protocol: each line is a request, answered with a line.
fn serve_lines<S: Read + Write>(stream: S, calls: &mpsc::Sender<Call>) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = answer(&line, calls);
        writeln!(reader.get_mut(), "{response}")?;
    }
}

/// Serves one HTTP request: `POST /ipc` with a JSON body, answered with the JSON response.
fn serve_http_request<S: Read + Write>(stream: S, calls: &mpsc::Sender<Call>) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut length = 0usize;
    let mut json_content = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        let header = header.to_ascii_lowercase();
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = header.strip_prefix("content-type:") {
            json_content = value.trim().starts_with("application/json");
        }
    }

    let (status, content_type, body) = if method != "POST" || path != "/ipc" {
        ("404 Not Found", "text/plain", "POST JSON requests to /ipc\n".to_owned())
    } else if !json_content {
        ("415 Unsupported Media Type", "text/plain", "send the request with Content-Type: application/json\n".to_owned())
    } else if length > MAX_BODY {
        ("413 Payload Too Large", "text/plain", "the request is too large\n".to_owned())
    } else {
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        ("200 OK", "application/json", answer(&String::from_utf8_lossy(&body), calls).to_string())
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    reader.get_mut().write_all(response.as_bytes())
}

/// Carries out one request's JSON text and returns the response to send back.
fn answer(text: &str, calls: &mpsc::Sender<Call>) -> Value {
    let message: Value = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(err) => return failure(Value::Null, "invalid_json", err.to_string()),
    };
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return failure(id, "invalid_request", "a request needs a method");
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let (reply, answer) = mpsc::channel();
    let call = Call { method: method.to_owned(), params, reply };
    if calls.send(call).is_err() {
        return failure(id, "not_running", "rsimgv is not running");
    }
    match answer.recv_timeout(ANSWER_TIMEOUT) {
        Ok(Ok(result)) => json!({"id": id, "result": result}),
        Ok(Err(error)) => json!({"id": id, "error": {"code": error.code, "message": error.message}}),
        Err(_) => failure(id, "timeout", "rsimgv did not answer in time"),
    }
}

fn failure(id: Value, code: &str, message: impl Into<String>) -> Value {
    json!({"id": id, "error": {"code": code, "message": message.into()}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_requests_are_answered_with_an_error() {
        let (calls, _receiver) = mpsc::channel();
        assert_eq!(answer("not json", &calls)["error"]["code"], "invalid_json");
        assert_eq!(answer(r#"{"id": 3}"#, &calls)["error"]["code"], "invalid_request");
        assert_eq!(answer(r#"{"id": 4, "method": "tabs.list"}"#, &calls)["error"]["code"], "not_running");
    }
}
