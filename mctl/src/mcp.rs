//! The one MCP server an AI agent on the master talks to: every worker's tools, each with a `machine` argument.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

use crate::Master;

const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn describe(entry: &mut Value, reached: Result<Value, crate::Failure>) {
    entry["reachable"] = json!(reached.is_ok());
    match reached {
        Ok(status) => {
            for field in ["identity", "os", "arch", "user", "elevated", "session", "version", "capabilities"] {
                entry[field] = status[field].clone();
            }
        }
        Err(failure) => entry["problem"] = json!(failure.message),
    }
}

/// The master's own machine first, then its workers.
fn machines(master: &Master) -> Value {
    let mut own = json!({ "name": master.own_name(), "role": "master" });
    describe(&mut own, master.run(master.own_name(), "status", &Value::Null));
    let workers = master.machines().into_iter().map(|machine| {
        let mut entry = json!({ "name": machine.name, "role": "worker", "address": machine.address });
        describe(&mut entry, master.call(&machine, "status", &Value::Null));
        entry
    });
    json!({ "machines": std::iter::once(own).chain(workers).collect::<Vec<_>>() })
}

/// Every tool, each given a `machine` argument. The tools are the same on the master and on every worker.
fn tools(master: &Master) -> Vec<Value> {
    let mut listed = vec![json!({
        "name": "machines",
        "description": "The machines this master can use: its own, and the workers paired with it. For each: name, system, whether it is reachable, and what works there. Pass a name as `machine` to the other tools.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
    }), json!({
        "name": "guide",
        "description": "How to use Master Control well, written for AI agents: the rules for positions, windows and element ids, working without disturbing the user, batches, observation, what each refusal means, and recipes. Read it once before a longer piece of work.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
    })];
    for mut tool in mc_agent::tool_list() {
        tool["inputSchema"]["properties"]["machine"] = json!({ "type": "string", "description": format!("The machine to do this on: a name from `machines` (\"{}\" is this machine itself).", master.own_name()) });
        let mut required = tool["inputSchema"]["required"].as_array().cloned().unwrap_or_default();
        required.insert(0, json!("machine"));
        tool["inputSchema"]["required"] = json!(required);
        tool["description"] = json!(format!("On the named machine: {}", tool["description"].as_str().unwrap_or_default()));
        listed.push(tool);
    }
    listed
}

fn handle(master: &Master, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str)?;
    let id = id?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            json!({
                "protocolVersion": VERSIONS.iter().find(|version| **version == asked).unwrap_or(&VERSIONS[0]),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "master-control", "title": "Master Control", "version": mc_core::VERSION },
                "instructions": "Master Control drives and observes this machine and the worker machines paired with it. Call `machines` first; every other tool takes a machine's name as `machine`. The `guide` tool explains how to use it well.",
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools(master) }),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else { return Some(error(&id, -32602, "tools/call needs the tool's name")) };
            let mut args = params["arguments"].clone();
            let outcome = if name == "machines" {
                Ok(machines(master))
            } else if name == "guide" {
                Ok(json!({ "guide": crate::agents::GUIDE }))
            } else {
                let machine = args.as_object_mut().and_then(|object| object.remove("machine"));
                match machine.as_ref().and_then(Value::as_str) {
                    None => Err(crate::Failure { code: "bad_input".into(), message: "machine is required: a name from `machines`".into() }),
                    Some(machine) => master.run(machine, name, &args),
                }
            };
            match outcome {
                Ok(mut result) => {
                    let picture = result.as_object_mut().and_then(|object| object.remove("png_base64"));
                    let mut content = vec![json!({ "type": "text", "text": result.to_string() })];
                    if let Some(data) = picture {
                        content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
                    }
                    json!({ "content": content, "structuredContent": result, "isError": false })
                }
                Err(failure) => json!({ "content": [{ "type": "text", "text": failure.to_string() }], "isError": true }),
            }
        }
        _ => return Some(error(&id, -32601, &format!("the method {method} isn't supported"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Serves MCP, one JSON message a line, until the input ends.
pub fn serve(master: &Master, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(master, &message),
            Err(_) => Some(error(&Value::Null, -32700, "the message isn't JSON")),
        };
        if let Some(answer) = answer {
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(())
}
