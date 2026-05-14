use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct WndSocketClient {
    socket: PathBuf,
    timeout: Duration,
}

impl WndSocketClient {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        if method.trim().is_empty() {
            bail!("socket method cannot be empty");
        }
        self.request_value(json!({
            "id": 1,
            "method": method,
            "params": params,
        }))
    }

    pub fn request_value(&self, request: Value) -> Result<Value> {
        request_json_line(&self.socket, self.timeout, &request)
    }
}

#[cfg(unix)]
pub fn request_json_line(socket: &Path, timeout: Duration, request: &Value) -> Result<Value> {
    use std::os::unix::net::UnixStream;

    let mut stream =
        UnixStream::connect(socket).with_context(|| format!("connecting {}", socket.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .context("setting socket read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("setting socket write timeout")?;
    serde_json::to_writer(&mut stream, request).context("writing JSON request")?;
    stream.write_all(b"\n").context("writing request newline")?;
    stream.flush().context("flushing request")?;

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader
        .read_line(&mut line)
        .context("reading JSON response")?;
    if line.trim().is_empty() {
        bail!("empty socket response");
    }
    serde_json::from_str(&line).context("parsing JSON response")
}

#[cfg(not(unix))]
pub fn request_json_line(_socket: &Path, _timeout: Duration, _request: &Value) -> Result<Value> {
    bail!("direct wnd socket mode is only available on Unix platforms")
}

pub fn render_socket_probe(socket: &Path, method: &str) -> String {
    let client = WndSocketClient::new(socket);
    match client.request(method, Value::Null) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        Err(error) => format!("socket probe failed: {error:#}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn sends_and_reads_json_lines() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("wnd.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "ping");
            stream.write_all(br#"{"id":1,"result":"pong"}"#).unwrap();
            stream.write_all(b"\n").unwrap();
        });

        let response = WndSocketClient::new(socket)
            .request("ping", Value::Null)
            .unwrap();
        assert_eq!(response["result"], "pong");
    }
}
