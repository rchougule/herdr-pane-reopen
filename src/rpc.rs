//! NDJSON-over-Unix-socket client for the herdr server.
//!
//! Hard constraints, verified on herdr 0.9.1 / protocol 22:
//! - ONE request per connection — the server closes the connection after the response.
//! - A connection with an active `events.subscribe` cannot serve requests.
//!
//! Therefore `Client` is connectionless: it holds the path and each `call()` does
//! connect → write one line → read one line → close.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// A structured `{"error":{"code","message"}}` reply.
    Remote {
        code: String,
        message: String,
    },
    /// Malformed or unexpected payload; never a panic.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Remote { code, message } => write!(f, "{code}: {message}"),
            Error::Protocol(m) => write!(f, "protocol: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl Error {
    /// A transport timeout — the request may still be executing on the server, so the
    /// caller must NOT assume it failed (F2).
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Error::Io(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock
        )
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Remote { code, .. } => Some(code.as_str()),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Parse one response line into the `result` object (or a remote error).
/// Pure — unit-tested against captured transcripts.
pub fn parse_response(line: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(line.trim())
        .map_err(|e| Error::Protocol(format!("bad json: {e}: {}", truncate(line, 200))))?;
    if let Some(err) = v.get("error") {
        return Err(Error::Remote {
            code: err
                .get("code")
                .and_then(|c| c.as_str())
                .unwrap_or("unknown")
                .to_string(),
            message: err
                .get("message")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    match v.get("result") {
        Some(r) => Ok(r.clone()),
        None => Err(Error::Protocol(format!(
            "no result field in {}",
            truncate(line, 200)
        ))),
    }
}

/// A line from a subscribed connection: either a response (has `id`) or an event
/// (`{"event": "...", "data": {...}}`, no `id`). Pure.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Response(Value),
    Event { event: String, data: Value },
    Unknown,
}

pub fn classify_frame(line: &str) -> Frame {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Frame::Unknown;
    };
    if v.get("id").is_some() {
        return Frame::Response(v);
    }
    if let Some(ev) = v.get("event").and_then(|e| e.as_str()) {
        return Frame::Event {
            event: ev.to_string(),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        };
    }
    Frame::Unknown
}

/// Truncate to at most `n` BYTES, never splitting a UTF-8 sequence. `&s[..n]` on a
/// multi-byte boundary panics, and this runs on exactly the malformed payloads the
/// caller is trying to survive (F3).
pub fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Default read/write timeout. herdr answers a local Unix socket in milliseconds, so a
/// call that has not answered in 5 s is not going to (F7). `agent.start` is the one
/// method that legitimately blocks for a long time and it passes its own timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the restore driver and the snapshot refresher need from the socket.
/// Factored out so both can be exercised against a fake server in unit tests (F11).
pub trait Rpc {
    fn call_with_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value>;

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, DEFAULT_TIMEOUT)
    }

    fn ok(&self, method: &str, params: Value) -> Result<()> {
        self.call(method, params).map(|_| ())
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    path: PathBuf,
    timeout: Duration,
}

impl Rpc for Client {
    fn call_with_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.call_inner(method, params, timeout)
    }
}

impl Client {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Client {
            path: path.as_ref().to_path_buf(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn call_inner(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut w = stream.try_clone()?;
        let req = json!({"id": "reopen", "method": method, "params": params});
        let mut line = serde_json::to_string(&req).map_err(|e| Error::Protocol(e.to_string()))?;
        line.push('\n');
        w.write_all(line.as_bytes())?;
        w.flush()?;
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        let n = reader.read_line(&mut resp)?;
        if n == 0 {
            return Err(Error::Protocol(format!("empty response to {method}")));
        }
        parse_response(&resp)
    }

    pub fn call_as<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let v = self.call_inner(method, params, self.timeout)?;
        serde_json::from_value(v).map_err(|e| Error::Protocol(format!("{method}: {e}")))
    }

    /// `ping` → (version, protocol)
    pub fn ping(&self) -> Result<(String, u32)> {
        let r = self.call_inner("ping", json!({}), self.timeout)?;
        Ok((
            r.get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            r.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        ))
    }

    /// Open a dedicated subscription connection. Requests must NOT be sent on it.
    pub fn subscribe(&self, types: &[&str]) -> Result<Subscription> {
        let stream = UnixStream::connect(&self.path)?;
        let mut w = stream.try_clone()?;
        let subs: Vec<Value> = types.iter().map(|t| json!({"type": t})).collect();
        let req =
            json!({"id":"reopen-sub","method":"events.subscribe","params":{"subscriptions":subs}});
        let mut line = serde_json::to_string(&req).map_err(|e| Error::Protocol(e.to_string()))?;
        line.push('\n');
        w.write_all(line.as_bytes())?;
        w.flush()?;
        let mut reader = BufReader::new(stream);
        let mut ack = String::new();
        reader.read_line(&mut ack)?;
        parse_response(&ack)?;
        Ok(Subscription { reader })
    }
}

pub struct Subscription {
    reader: BufReader<UnixStream>,
}

impl Subscription {
    /// Blocking read of the next frame. `None` on EOF.
    pub fn next_frame(&mut self) -> Option<Frame> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(classify_frame(&line)),
        }
    }
}
