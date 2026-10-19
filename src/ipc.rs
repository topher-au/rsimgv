//! The IPC: JSON requests that drive a running rsimgv, over a Unix socket, a TCP socket, or HTTP.
//!
//! A request is `{"id": 1, "method": "tabs.list", "params": {}}`, and its answer is either
//! `{"id": 1, "result": {...}}` or `{"id": 1, "error": {"code": "...", "message": "..."}}`.
//!
//! - The Unix socket and the TCP socket take one request per line, and answer with one line per request.
//!   The TCP socket listens on 127.0.0.1 only.
//! - HTTP takes a request as the body of `POST /ipc`, sent with `Content-Type: application/json`, and answers
//!   with the response as the body. A request without that content type is refused: a web page cannot send
//!   it cross-site without a preflight request, which is never answered.
//!
//! The socket is `rsimgv.sock` in the user's runtime directory, with mode 0600. Methods are in `remote`.
//! Requests are carried out on the GTK thread, where the window is.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gtk::glib;
use serde_json::{json, Value};

/// How long a transport waits for the GTK thread to answer before it gives up.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest HTTP body accepted, in bytes.
const MAX_BODY: usize = 1 << 20;

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

/// Which transports to serve. A TCP or HTTP port of 0 leaves that transport off.
pub struct Config {
    pub socket: bool,
    pub tcp_port: u16,
    pub http_port: u16,
}

impl Config {
    pub fn any_enabled(&self) -> bool {
        self.socket || self.tcp_port != 0 || self.http_port != 0
    }
}

/// Where the Unix socket is: `rsimgv.sock` in the user's runtime directory.
pub fn socket_path() -> PathBuf {
    glib::user_runtime_dir().join("rsimgv.sock")
}

/// Starts each enabled transport and returns the channel its calls arrive on. A transport that cannot start is
/// reported, and the others still run.
pub fn start(config: &Config) -> mpsc::Receiver<Call> {
    let (calls, receiver) = mpsc::channel();
    if config.socket {
        if let Err(err) = serve_unix(&socket_path(), calls.clone()) {
            eprintln!("rsimgv: could not open the IPC socket: {err}");
        }
    }
    if config.tcp_port != 0 {
        if let Err(err) = serve_tcp(config.tcp_port, calls.clone()) {
            eprintln!("rsimgv: could not open the IPC TCP port {}: {err}", config.tcp_port);
        }
    }
    if config.http_port != 0 {
        if let Err(err) = serve_http(config.http_port, calls.clone()) {
            eprintln!("rsimgv: could not open the IPC HTTP port {}: {err}", config.http_port);
        }
    }
    receiver
}

fn serve_unix(path: &std::path::Path, calls: mpsc::Sender<Call>) -> io::Result<()> {
    // A socket left by an earlier run would stop the bind, so it goes first.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let calls = calls.clone();
            thread::spawn(move || {
                let _ = serve_lines(stream, &calls);
            });
        }
    });
    Ok(())
}

fn serve_tcp(port: u16, calls: mpsc::Sender<Call>) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let calls = calls.clone();
            thread::spawn(move || {
                let _ = serve_lines(stream, &calls);
            });
        }
    });
    Ok(())
}

fn serve_http(port: u16, calls: mpsc::Sender<Call>) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let calls = calls.clone();
            thread::spawn(move || {
                let _ = serve_http_request(stream, &calls);
            });
        }
    });
    Ok(())
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
