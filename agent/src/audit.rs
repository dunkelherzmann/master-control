//! The agent's own record of what it was asked to do (specs.md §5): one JSON line per request.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::tools::ToolError;

pub struct Audit {
    file: Mutex<File>,
}

impl Audit {
    pub fn open(path: &Path) -> io::Result<Audit> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        crate::state::restrict_to_user(path)?;
        Ok(Audit { file: Mutex::new(file) })
    }

    pub fn record(&self, via: &str, tool: &str, args: &Value, error: Option<&ToolError>, took: Duration) {
        let line = json!({
            "time_ms": now_ms(),
            "via": via,
            "tool": tool,
            "args": summarise(args),
            "ok": error.is_none(),
            "error": error.map(|error| error.code),
            "took_ms": took.as_millis() as u64,
        });
        if let Ok(mut file) = self.file.lock() {
            // A full disk must not stop the agent answering; the next line tries again.
            let _ = writeln!(file, "{line}");
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// The arguments as they go into the log: file contents, input and environment values are left out, since they can
/// hold secrets and can be large. What remains says what was done and to what.
fn summarise(args: &Value) -> Value {
    let Some(object) = args.as_object() else { return Value::Null };
    let mut kept = Map::new();
    for (name, value) in object {
        let entry = match (name.as_str(), value) {
            ("content_base64" | "text" | "stdin" | "value", Value::String(text)) => json!({ "omitted_chars": text.len() }),
            // A batch's steps are each written on their own line as they run; here only which tools they are.
            ("steps", Value::Array(steps)) => json!({ "tools": steps.iter().map(|step| step["tool"].clone()).collect::<Vec<_>>() }),
            ("env", Value::Object(variables)) => json!({ "names": variables.keys().collect::<Vec<_>>() }),
            (_, Value::String(text)) if text.len() > 2000 => {
                let cut = (0..=2000).rev().find(|&index| text.is_char_boundary(index)).unwrap_or(0);
                json!(format!("{}… ({} chars)", &text[..cut], text.len()))
            }
            _ => value.clone(),
        };
        kept.insert(name.clone(), entry);
    }
    Value::Object(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_and_environment_values_stay_out_of_the_log() {
        let args = json!({
            "path": "~/x.txt",
            "content_base64": "c2VjcmV0",
            "stdin": "password",
            "text": "hidden-text",
            "value": "hidden-value",
            "steps": [{"tool": "type_text", "args": {"text": "hidden-in-a-step"}}],
            "env": {"API_KEY": "abc"},
            "command": "echo hi",
        });
        let text = summarise(&args).to_string();
        assert!(text.contains("~/x.txt") && text.contains("echo hi") && text.contains("API_KEY"));
        assert!(!text.contains("hidden-value") && !text.contains("hidden-in-a-step") && text.contains("type_text"), "{text}");
        assert!(!text.contains("c2VjcmV0") && !text.contains("password") && !text.contains("abc") && !text.contains("hidden-text"));
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let args = json!({ "command": "é".repeat(3000) });
        let text = summarise(&args)["command"].as_str().unwrap().to_string();
        assert!(text.ends_with("(6000 chars)") && text.len() < 2100);
    }
}
