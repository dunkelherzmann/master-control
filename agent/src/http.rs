//! The HTTP API (specs.md §5): loopback only, a token on every request.
//!
//! - `POST /v1/<tool>` with a JSON object of arguments; `GET /v1/status` works too.
//! - `PUT /v1/files?path=…` writes the request body to a file; `GET /v1/files?path=…` returns a file's bytes.
//! - `POST /mcp` speaks MCP (JSON-RPC), answering in JSON.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::tools::{self, ToolError};
use crate::{mcp, state, Agent};

/// The largest request body accepted.
const BODY_LIMIT: usize = 256 * 1024 * 1024;

pub fn serve(agent: Arc<Agent>, port: u16, stop: Arc<AtomicBool>) -> io::Result<(u16, JoinHandle<()>)> {
    let server = Server::http(("127.0.0.1", port)).map_err(|error| io::Error::other(format!("can't listen on port {port}: {error}")))?;
    let port = server.server_addr().to_ip().map_or(port, |address| address.port());
    let thread = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(request)) => {
                    // A thread per request: one long `exec` must not hold up the others.
                    let agent = agent.clone();
                    std::thread::spawn(move || handle(&agent, request));
                }
                Ok(None) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
    Ok((port, thread))
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request.headers().iter().find(|header| header.field.equiv(name)).map(|header| header.value.as_str())
}

/// Compares without stopping at the first difference, so timing says nothing about the token.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |difference, (x, y)| difference | (x ^ y)) == 0
}

/// Why a request is refused before it reaches a tool, if it is.
fn refusal(agent: &Agent, request: &Request) -> Option<(u16, &'static str, &'static str)> {
    // A page in a browser can reach a loopback port. It can't set the token, but refuse it by its marks too: a Host
    // that isn't this machine (DNS rebinding) or an Origin header.
    let host = header(request, "Host").unwrap_or_default();
    let name = host.rsplit_once(':').map_or(host, |(name, port)| if port.bytes().all(|byte| byte.is_ascii_digit()) { name } else { host });
    if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        return Some((403, "forbidden", "the agent answers only on the loopback address"));
    }
    if header(request, "Origin").is_some() {
        return Some((403, "forbidden", "requests from web pages are refused"));
    }
    let given = header(request, "Authorization").and_then(|value| value.strip_prefix("Bearer ")).unwrap_or_default();
    if !same(given.trim().as_bytes(), agent.token.as_bytes()) {
        return Some((401, "unauthorized", "a valid token is required: Authorization: Bearer <token>"));
    }
    None
}

fn respond(request: Request, status: u16, content_type: &str, body: Vec<u8>) {
    let header = Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("a fixed, valid header");
    let _ = request.respond(Response::from_data(body).with_status_code(status).with_header(header));
}

fn respond_json(request: Request, status: u16, body: &Value) {
    respond(request, status, "application/json", body.to_string().into_bytes());
}

fn respond_error(request: Request, status: u16, code: &str, message: &str) {
    respond_json(request, status, &json!({ "error": { "code": code, "message": message } }));
}

fn respond_tool_error(request: Request, error: &ToolError) {
    let status = match error.code {
        "bad_input" => 400,
        "not_found" | "unknown_tool" | "window_not_found" => 404,
        "focus_refused" | "element_gone" => 409,
        "timed_out" => 408,
        "not_supported" => 501,
        _ => 500,
    };
    respond_error(request, status, error.code, &error.message);
}

fn body(request: &mut Request) -> Result<Vec<u8>, (u16, &'static str, &'static str)> {
    if request.body_length().is_some_and(|length| length > BODY_LIMIT) {
        return Err((413, "too_large", "the request body is too large"));
    }
    let mut bytes = Vec::new();
    match request.as_reader().take(BODY_LIMIT as u64 + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() > BODY_LIMIT => Err((413, "too_large", "the request body is too large")),
        Ok(_) => Ok(bytes),
        Err(_) => Err((400, "bad_input", "the request body couldn't be read")),
    }
}

fn handle(agent: &Agent, mut request: Request) {
    if let Some((status, code, message)) = refusal(agent, &request) {
        return respond_error(request, status, code, message);
    }
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let method = request.method().clone();
    match (&method, path) {
        (Method::Get, "/v1/status") => match agent.call("http", "status", &Value::Null) {
            Ok(result) => respond_json(request, 200, &result),
            Err(error) => respond_tool_error(request, &error),
        },
        (Method::Put | Method::Get, "/v1/files") => files(agent, request, &method, query),
        (Method::Post, "/mcp") => {
            let bytes = match body(&mut request) {
                Ok(bytes) => bytes,
                Err((status, code, message)) => return respond_error(request, status, code, message),
            };
            match mcp::handle_bytes(agent, "mcp", &bytes) {
                Some(answer) => respond_json(request, 200, &answer),
                // Only notifications came: accepted, nothing to say.
                None => respond(request, 202, "application/json", Vec::new()),
            }
        }
        (_, "/mcp") => respond_error(request, 405, "method_not_allowed", "MCP here is POST only; answers come as JSON"),
        (Method::Post, _) if path.starts_with("/v1/") => {
            let tool = &path["/v1/".len()..];
            let bytes = match body(&mut request) {
                Ok(bytes) => bytes,
                Err((status, code, message)) => return respond_error(request, status, code, message),
            };
            let args = if bytes.iter().all(u8::is_ascii_whitespace) {
                Value::Null
            } else {
                match serde_json::from_slice(&bytes) {
                    Ok(args) => args,
                    Err(error) => return respond_error(request, 400, "bad_input", &format!("the body isn't JSON: {error}")),
                }
            };
            match agent.call("http", tool, &args) {
                Ok(result) => respond_json(request, 200, &result),
                Err(error) => respond_tool_error(request, &error),
            }
        }
        _ if path.starts_with("/v1/") => respond_error(request, 405, "method_not_allowed", "tools are called with POST"),
        _ => respond_error(request, 404, "not_found", "no such route"),
    }
}

/// Whole files as raw bytes, for what is too big to send as base64 in JSON.
fn files(agent: &Agent, mut request: Request, method: &Method, query: &str) {
    let Some(path) = query.split('&').find_map(|pair| pair.strip_prefix("path=")).and_then(percent_decode).filter(|path| !path.is_empty())
    else {
        return respond_error(request, 400, "bad_input", "add ?path=… (percent-encoded)");
    };
    let resolved = state::resolve(&path);
    let began = std::time::Instant::now();
    let (tool, result) = if *method == Method::Put {
        let bytes = match body(&mut request) {
            Ok(bytes) => bytes,
            Err((status, code, message)) => return respond_error(request, status, code, message),
        };
        ("push", tools::write_file(&resolved, &bytes).map(|answer| (answer.to_string().into_bytes(), "application/json")))
    } else {
        let read = tools::open_file(&resolved).and_then(|(mut file, _)| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|error| ToolError { code: "failed", message: format!("{path}: {error}") })?;
            Ok((bytes, "application/octet-stream"))
        });
        ("pull", read)
    };
    agent.audit.record("http", tool, &json!({ "path": path, "raw": true }), result.as_ref().err(), began.elapsed());
    match result {
        Ok((bytes, content_type)) => respond(request, 200, content_type, bytes),
        Err(error) => respond_tool_error(request, &error),
    }
}

/// Decodes `%41` and `+` in a query value. None when it isn't valid.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let pair = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
                decoded.push(u8::from_str_radix(pair, 16).ok()?);
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_values() {
        assert_eq!(percent_decode("C%3A%5CUsers%5Ca+b.txt").as_deref(), Some("C:\\Users\\a b.txt"));
        assert_eq!(percent_decode("%C3%A9").as_deref(), Some("é"));
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%ff"), None);
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
        assert!(!same(b"", b"a"));
    }
}
