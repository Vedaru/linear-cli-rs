//! Shared integration-test support: a headless mock of the Linear GraphQL API
//! and a helper for running the compiled `linear` binary against it.
//!
//! Port of `test/utils/mock_linear_server.ts` and `test/utils/test-helpers.ts`.
//! The server is deliberately dependency-free (std TCP only) so tests do not
//! need an async runtime or an extra dev-dependency.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::{json, Map, Value};

/// One canned GraphQL response. Mirrors `MockResponse` upstream.
#[derive(Clone, Debug)]
pub struct MockResponse {
    pub query_name: String,
    pub query_includes: Option<String>,
    /// When set, every entry must deep-equal the request variable of the same
    /// name. When `None`, any variables match.
    pub variables: Option<Map<String, Value>>,
    pub response: Value,
    pub status: u16,
}

impl MockResponse {
    pub fn new(query_name: impl Into<String>, response: Value) -> Self {
        Self {
            query_name: query_name.into(),
            query_includes: None,
            variables: None,
            response,
            status: 200,
        }
    }

    pub fn with_variables(mut self, variables: Value) -> Self {
        self.variables = variables.as_object().cloned();
        self
    }

    pub fn with_query_includes(mut self, needle: impl Into<String>) -> Self {
        self.query_includes = Some(needle.into());
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

/// A signed-URL file upload captured by the server (the PUT step).
#[derive(Clone, Debug)]
pub struct UploadRequest {
    pub pathname: String,
    pub content_type: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct ServerState {
    responses: Vec<MockResponse>,
    uploads: Vec<UploadRequest>,
}

/// A mock Linear API server bound to an ephemeral port on 127.0.0.1.
pub struct MockLinearServer {
    addr: SocketAddr,
    state: Arc<Mutex<ServerState>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockLinearServer {
    pub fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(Mutex::new(ServerState {
            responses,
            uploads: Vec::new(),
        }));
        let running = Arc::new(AtomicBool::new(true));

        let thread_state = Arc::clone(&state);
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if !thread_running.load(Ordering::SeqCst) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let state = Arc::clone(&thread_state);
                        thread::spawn(move || {
                            let _ = handle_connection(stream, &state);
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            state,
            running,
            handle: Some(handle),
        }
    }

    pub fn get_endpoint(&self) -> String {
        format!("http://{}/graphql", self.addr)
    }

    /// URL handed out as a `fileUpload` signed upload URL in mock responses.
    pub fn get_upload_url(&self) -> String {
        format!("http://{}/upload", self.addr)
    }

    /// Uploads received so far, in arrival order.
    pub fn uploads(&self) -> Vec<UploadRequest> {
        self.state.lock().unwrap().uploads.clone()
    }

    pub fn add_response(&self, response: MockResponse) {
        self.state.lock().unwrap().responses.push(response);
    }

    pub fn clear_responses(&self) {
        self.state.lock().unwrap().responses.clear();
    }
}

impl Drop for MockLinearServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Unblock the accept loop with a throwaway connection.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    state: &Arc<Mutex<ServerState>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let (status, payload) = route(&method, &path, &headers, &body, state);
    write_response(&mut stream, status, &payload)
}

fn route(
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
    state: &Arc<Mutex<ServerState>>,
) -> (u16, Value) {
    if method == "OPTIONS" {
        return (200, Value::Null);
    }

    if method == "POST" && path == "/graphql" {
        return handle_graphql(body, state);
    }

    if method == "PUT" && path.starts_with("/upload") {
        let mut guard = state.lock().unwrap();
        guard.uploads.push(UploadRequest {
            pathname: path.to_string(),
            content_type: headers.get("content-type").cloned(),
            headers: headers.clone(),
            body: body.to_vec(),
        });
        return (200, Value::Null);
    }

    (404, json!("Not Found"))
}

fn handle_graphql(body: &[u8], state: &Arc<Mutex<ServerState>>) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            return (
                400,
                json!({ "errors": [{ "message": "Invalid JSON in request body",
                                     "extensions": { "code": "BAD_REQUEST" } }] }),
            );
        }
    };

    let query = parsed.get("query").and_then(Value::as_str).unwrap_or("");
    let variables = parsed.get("variables").cloned().unwrap_or(Value::Null);
    let query_name = extract_query_name(query);

    let guard = state.lock().unwrap();
    let matched = guard.responses.iter().find(|mock| {
        mock.query_name == query_name
            && match &mock.query_includes {
                None => true,
                Some(needle) => query.contains(needle),
            }
            && match &mock.variables {
                None => true,
                Some(expected) => expected.iter().all(|(key, value)| {
                    deep_equal(variables.get(key).unwrap_or(&Value::Null), value)
                }),
            }
    });

    match matched {
        Some(mock) => (mock.status, mock.response.clone()),
        None => (
            200,
            json!({ "errors": [{
                "message": "No mock response configured for this query",
                "extensions": {
                    "code": "NO_MOCK_CONFIGURED",
                    "query": query_name,
                    "variables": variables,
                }
            }] }),
        ),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, payload: &Value) -> std::io::Result<()> {
    let body = if payload.is_null() {
        String::new()
    } else {
        payload.to_string()
    };
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 GMT\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

fn extract_query_name(query: &str) -> String {
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let rest = &query[i..];
            for keyword in ["query", "mutation", "subscription"] {
                if let Some(after) = rest.strip_prefix(keyword) {
                    let name: String = after
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        return name;
                    }
                }
            }
            // Skip the rest of this identifier.
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    "UnknownQuery".to_string()
}

fn deep_equal(a: &Value, b: &Value) -> bool {
    a == b
}

/// Result of running the compiled `linear` binary.
#[derive(Clone, Debug)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

impl CliOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Render in the shape the upstream snapshot tests record.
    pub fn snapshot(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            quote_string(&self.stdout),
            quote_string(&self.stderr)
        )
    }
}

fn quote_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Run the `linear` binary with the given args and environment.
///
/// `NO_COLOR` is forced on unless the caller overrides it, matching the
/// upstream snapshot harness.
pub fn run_cli(args: &[&str], env: &[(String, String)]) -> CliOutput {
    run_cli_stdin(args, env, None)
}

pub fn run_cli_stdin(args: &[&str], env: &[(String, String)], stdin: Option<&str>) -> CliOutput {
    run_cli_full(args, env, &[], stdin)
}

/// Like [`run_cli_stdin`] but also removes `remove` variables from the child
/// environment, so tests can assert the "nothing configured" paths regardless
/// of the developer's ambient environment.
pub fn run_cli_full(
    args: &[&str],
    env: &[(String, String)],
    remove: &[&str],
    stdin: Option<&str>,
) -> CliOutput {
    let mut command = Command::new(env!("CARGO_BIN_EXE_linear"));
    command.args(args);
    command.env("NO_COLOR", "1");
    for key in remove {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().expect("spawn linear binary");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }

    let output = child.wait_with_output().expect("wait for linear");
    CliOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
    }
}

/// Environment that points the CLI at `server` and supplies a token.
///
/// HOME and XDG_CONFIG_HOME are redirected to a scratch directory because the CLI
/// resolves `linear/linear.toml` through either of them. On a machine where the
/// developer has run `linear auth login`, these suites otherwise read *their*
/// workspace: a mock answers no request it was not told about ("No mock response
/// configured for this query"), and a test that asserts "no API key configured"
/// finds one. Both are failures of the test setup, not of the command under test.
pub fn mock_env(server: &MockLinearServer) -> Vec<(String, String)> {
    static SCRATCH: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let home = SCRATCH
        .get_or_init(|| tempfile::tempdir().expect("scratch home"))
        .path();
    vec![
        ("HOME".to_string(), home.display().to_string()),
        (
            "XDG_CONFIG_HOME".to_string(),
            home.join(".config").display().to_string(),
        ),
        ("LINEAR_GRAPHQL_ENDPOINT".to_string(), server.get_endpoint()),
        ("LINEAR_API_KEY".to_string(), "test-token".to_string()),
        ("LINEAR_IGNORE_ENV_FILE".to_string(), "1".to_string()),
    ]
}

pub const ENG_TEAM_ID: &str = "team-eng-id";
pub const ENG_TEAM_KEY: &str = "ENG";
pub const ENG_TEAM_NAME: &str = "Engineering";

/// Mock for the shared `ResolveTeam` key/name lookup.
pub fn resolve_team_mock(reference: &str) -> MockResponse {
    MockResponse::new(
        "ResolveTeam",
        json!({ "data": { "teams": { "nodes": [{
            "id": ENG_TEAM_ID, "key": ENG_TEAM_KEY, "name": ENG_TEAM_NAME
        }] } } }),
    )
    .with_variables(json!({ "reference": reference }))
}
