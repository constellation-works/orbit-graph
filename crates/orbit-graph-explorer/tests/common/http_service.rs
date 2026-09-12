//! Shared real-binary launcher for the loopback service.
//!
//! Every caller launches the packaged `orbit-graph-explorer` executable,
//! reads the per-launch bearer token from its standard error, and drives it
//! over a real TCP socket with hand-written HTTP/1.1 requests. Nothing here
//! asserts against an in-process handler: the contract under test is what the
//! shipped binary serves on the wire.

#![allow(dead_code)]
#![allow(clippy::expect_used)]

use std::any::Any;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::corpus;

/// How long to wait for indexing to finish before failing a test.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// A launched `orbit-graph-explorer serve` process.
pub struct Service {
    child: Mutex<Child>,
    pid: u32,
    _stderr: BufReader<ChildStderr>,
    pub origin: String,
    authority: String,
    pub token: String,
    pub repository: String,
    pub base_sha: String,
    pub head_sha: String,
    /// Keeps a fixture's backing directory (a `corpus::CorpusCase` or a
    /// standalone `tempfile::TempDir`) alive for the service's lifetime.
    _guard: Box<dyn Any>,
}

impl Service {
    pub fn launch(case_id: &str) -> Self {
        let case = corpus::build_case(case_id);
        let repository = case.repository.clone();
        let base_sha = case.base().to_string();
        let head_sha = case.head().to_string();
        Self::launch_at(repository, base_sha, head_sha, Box::new(case))
    }

    /// Launch against an arbitrary repository and pair of revisions, keeping
    /// `guard` alive for as long as the service runs. Used for fixtures the
    /// shared corpus builder does not produce, such as an ad hoc repository
    /// exercising an unusual file name.
    pub fn launch_at(
        repository: PathBuf,
        base_sha: String,
        head_sha: String,
        guard: Box<dyn Any>,
    ) -> Self {
        let repository = repository.canonicalize().unwrap_or(repository);

        let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
            .args([
                "serve",
                "--repo",
                repository.to_str().expect("utf8 repository path"),
                "--base",
                base_sha.as_str(),
                "--head",
                head_sha.as_str(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch orbit-graph-explorer serve");

        let stderr = child.stderr.take().expect("capture stderr");
        let (mut stderr, origin, token) = read_launch_banner(stderr);
        // Keep the pipe open for the service's lifetime: closing the read end
        // would make any later write to the child's standard error fail.
        let _ = stderr.fill_buf();
        let authority = origin
            .strip_prefix("http://")
            .expect("loopback origin")
            .to_string();
        assert!(
            authority.starts_with("127.0.0.1:"),
            "the service must bind loopback only: {origin}"
        );

        Self {
            pid: child.id(),
            child: Mutex::new(child),
            _stderr: stderr,
            origin,
            authority,
            token,
            repository: repository.to_string_lossy().into_owned(),
            base_sha,
            head_sha,
            _guard: guard,
        }
    }

    pub fn authorized(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
        let authorization = format!("Bearer {}", self.token);
        let mut all = vec![("Authorization", authorization.as_str())];
        all.extend_from_slice(headers);
        self.request(method, path, all.as_slice())
    }

    pub fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
        let mut stream = TcpStream::connect(self.authority.as_str()).expect("connect to service");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set read timeout");
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.authority);
        for (field, value) in headers {
            request.push_str(format!("{field}: {value}\r\n").as_str());
        }
        request.push_str("Content-Length: 0\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("write request");
        stream.flush().expect("flush request");

        // Read exactly `Content-Length` body bytes rather than reading to
        // end-of-stream, so a connection close cannot discard an already
        // delivered response.
        let mut response = read_response(&mut stream)
            .unwrap_or_else(|error| panic!("`{method} {path}`: {error}; {}", self.diagnose()));
        response.request_line = format!("{method} {path}");
        response
    }

    /// Describe the service process, for a failure message that distinguishes
    /// a transport hiccup from a service that died.
    fn diagnose(&self) -> String {
        let mut child = Command::new("kill")
            .arg("-0")
            .arg(self.pid.to_string())
            .status();
        let alive = matches!(&mut child, Ok(status) if status.success());
        format!(
            "service pid {} is {}; origin {}",
            self.pid,
            if alive { "alive" } else { "gone" },
            self.origin
        )
    }

    pub fn wait_until_ready(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let health = self.authorized("GET", "/api/health", &[]).json();
            match health["indexing_status"].as_str() {
                Some("ready") => return,
                Some("failed") => panic!("indexing failed: {health}"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "indexing did not finish within {READY_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read the launch banner: the origin and the per-launch token, printed once.
///
/// Returns the reader so the caller can keep the pipe open.
fn read_launch_banner(stderr: ChildStderr) -> (BufReader<ChildStderr>, String, String) {
    let mut reader = BufReader::new(stderr);
    let mut origin = None;
    let mut token = None;
    for _ in 0..8 {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read launch banner");
        assert!(read > 0, "the service exited before printing its banner");
        if let Some(rest) = line
            .trim()
            .strip_prefix("orbit-graph-explorer listening on ")
        {
            origin = Some(rest.to_string());
        }
        if let Some(rest) = line.trim().strip_prefix("Authorization: Bearer ") {
            token = Some(rest.to_string());
        }
        if origin.is_some() && token.is_some() {
            break;
        }
    }
    let origin = origin.expect("launch banner names the service origin");
    let token = token.expect("launch banner prints the per-launch token");
    assert_eq!(token.len(), 64, "the token must carry 32 bytes of entropy");
    assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()), "{token}");
    (reader, origin, token)
}

/// Read one HTTP/1.1 response: the status line, the headers, and exactly the
/// number of body bytes the response declares.
fn read_response(stream: &mut TcpStream) -> Result<HttpResponse, String> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|error| format!("read status line: {error}"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?;

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("read header line: {error}"))?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((field, value)) = line.split_once(':') {
            headers.push((field.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    let length: usize = headers
        .iter()
        .find(|(field, _)| field == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader
        .read_exact(body.as_mut_slice())
        .map_err(|error| format!("read {length}-byte body: {error}"))?;

    Ok(HttpResponse {
        status,
        headers,
        body: String::from_utf8_lossy(body.as_slice()).into_owned(),
        request_line: String::new(),
    })
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub request_line: String,
}

impl HttpResponse {
    pub fn header(&self, field: &str) -> String {
        self.headers
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(self.body.as_str()).unwrap_or_else(|error| {
            panic!(
                "`{}` -> {} body is not JSON ({error}): {:?}",
                self.request_line, self.status, self.body
            )
        })
    }
}

/// Percent-encode a value for use in a query string.
pub fn percent_encode(raw: &str) -> String {
    let mut encoded = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(format!("%{byte:02X}").as_str());
        }
    }
    encoded
}
