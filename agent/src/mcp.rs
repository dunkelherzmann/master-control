//! MCP: the same tools as JSON-RPC, over HTTP (`POST /mcp`) or the agent's own stdin and stdout (`mc-agent mcp`).

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use crate::tools::TOOLS;
use crate::Agent;

/// The protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Answers one request. None for a notification, which gets no answer.
fn handle_one(agent: &Agent, via: &str, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to something we never asked, or not a request at all.
        return message.get("result").or(message.get("error")).is_none().then(|| error(&id.unwrap_or(Value::Null), -32600, "not a request"));
    };
    let id = id?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS.iter().find(|version| **version == asked).unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "master-control", "title": "Master Control", "version": crate::VERSION },
                "instructions": "Master Control runs on the machine under test. Call `status` first to see what works here.",
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": TOOLS
                .iter()
                .map(|tool| json!({ "name": tool.name, "description": tool.description, "inputSchema": (tool.schema)() }))
                .collect::<Vec<_>>(),
        }),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Some(error(&id, -32602, "tools/call needs the tool's name"));
            };
            if !TOOLS.iter().any(|tool| tool.name == name) {
                return Some(error(&id, -32602, &format!("there is no tool named {name}")));
            }
            // A tool that fails is still an answer: the model reads the message and can try again.
            match agent.call(via, name, &params["arguments"]) {
                Ok(mut result) => {
                    // A picture goes as an image the model can look at, not as base64 inside the text.
                    let picture = result.as_object_mut().and_then(|object| object.remove("png_base64"));
                    let mut content = vec![json!({ "type": "text", "text": result.to_string() })];
                    if let Some(data) = picture {
                        content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
                    }
                    json!({ "content": content, "structuredContent": result, "isError": false })
                }
                Err(failure) => json!({
                    "content": [{ "type": "text", "text": failure.to_string() }],
                    "isError": true,
                }),
            }
        }
        _ => return Some(error(&id, -32601, &format!("the method {method} isn't supported"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Answers a request body: one message, or a batch. None when nothing in it needs an answer.
pub fn handle_bytes(agent: &Agent, via: &str, bytes: &[u8]) -> Option<Value> {
    match serde_json::from_slice::<Value>(bytes) {
        Err(_) => Some(error(&Value::Null, -32700, "the message isn't JSON")),
        Ok(Value::Array(messages)) if messages.is_empty() => Some(error(&Value::Null, -32600, "an empty batch")),
        Ok(Value::Array(messages)) => {
            let answers: Vec<Value> = messages.iter().filter_map(|message| handle_one(agent, via, message)).collect();
            (!answers.is_empty()).then_some(Value::Array(answers))
        }
        Ok(message) => handle_one(agent, via, &message),
    }
}

/// Serves MCP on the given input and output, one JSON message a line, until the input ends.
pub fn serve_stdio(agent: &Agent, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = handle_bytes(agent, "stdio", line.as_bytes()) {
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(())
}
