//! The tools, with the same names in MCP, HTTP and the command line (specs.md §3.1).

use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

use crate::jobs::{self, Ended, Job, Spec, Stream};
use crate::{platform, state, Agent};

/// The most output or file content one answer carries; the rest is fetched with another call.
const CHUNK: usize = 256 * 1024;
const DEFAULT_TIMEOUT_S: u64 = 120;
const LONGEST_TIMEOUT_S: u64 = 3600;
const LONGEST_WAIT_MS: u64 = 60_000;

#[derive(Debug)]
pub struct ToolError {
    /// `bad_input`, `not_found`, `failed`, `unknown_tool`; from the desktop tools also `window_not_found`,
    /// `focus_refused`, `element_gone`, `timed_out` and `not_supported`.
    pub code: &'static str,
    pub message: String,
}

impl ToolError {
    pub(crate) fn bad(message: impl Into<String>) -> ToolError {
        ToolError { code: "bad_input", message: message.into() }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> ToolError {
        ToolError { code: "not_found", message: message.into() }
    }

    pub(crate) fn failed(message: impl Into<String>) -> ToolError {
        ToolError { code: "failed", message: message.into() }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// The arguments, as a JSON Schema object.
    pub schema: fn() -> Value,
    run: fn(&Agent, &Value) -> Result<Value, ToolError>,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "status",
        description: "The agent's version, the machine, the session it runs in, whether it has admin rights, and which \
                      capabilities work here.",
        schema: || object(json!({}), &[]),
        run: status,
    },
    Tool {
        name: "exec",
        description: "Run a command on the machine and wait for it. Give `argv` (a program and its arguments, run as \
                      given) or `command` (a line for the system's shell: sh, or cmd on Windows). Returns the exit \
                      code and the end of stdout and stderr. With `detach` it returns a job id at once; follow it \
                      with `job`. A command that runs past `timeout_s` is stopped.",
        schema: || {
            object(
                json!({
                    "argv": {"type": "array", "items": {"type": "string"}, "description": "Program and arguments."},
                    "command": {"type": "string", "description": "A shell command line, instead of argv."},
                    "cwd": {"type": "string", "description": "Working folder. Default: the user's home."},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Extra environment variables."},
                    "stdin": {"type": "string", "description": "Text to send to the command's input."},
                    "timeout_s": {"type": "integer", "description": "Seconds before it is stopped. Default 120, at most 3600. Ignored with detach."},
                    "detach": {"type": "boolean", "description": "Return a job id instead of waiting."},
                    "elevated": {"type": "boolean", "description": "Run with admin rights: through sudo on Linux and macOS; on Windows the agent itself must be running elevated."}
                }),
                &[],
            )
        },
        run: exec,
    },
    Tool {
        name: "job",
        description: "A job's state and its output from the given positions. `wait_ms` waits that long for it to end \
                      first. Pass `stdout_next` and `stderr_next` back as `stdout_from` and `stderr_from` to read on.",
        schema: || {
            object(
                json!({
                    "id": {"type": "integer"},
                    "stdout_from": {"type": "integer", "description": "Byte position to read stdout from. Default 0."},
                    "stderr_from": {"type": "integer", "description": "Byte position to read stderr from. Default 0."},
                    "wait_ms": {"type": "integer", "description": "Wait up to this long for the job to end. At most 60000."}
                }),
                &["id"],
            )
        },
        run: job,
    },
    Tool {
        name: "jobs",
        description: "Every job started since the agent started, with its state.",
        schema: || object(json!({}), &[]),
        run: list_jobs,
    },
    Tool {
        name: "kill",
        description: "Stop a running job and everything it started.",
        schema: || object(json!({"id": {"type": "integer"}}), &["id"]),
        run: kill,
    },
    Tool {
        name: "push",
        description: "Write a file on the machine. `~` is the user's home. The folder is created if missing; an \
                      existing file is replaced. For files over a few megabytes use HTTP PUT /v1/files instead.",
        schema: || {
            object(
                json!({
                    "path": {"type": "string"},
                    "content_base64": {"type": "string", "description": "The file's bytes, base64."},
                    "text": {"type": "string", "description": "The file's content as text, instead of content_base64."}
                }),
                &["path"],
            )
        },
        run: push,
    },
    Tool {
        name: "pull",
        description: "Read a file from the machine, up to 256 KB a call. Returns base64, the file's size and whether \
                      the end was reached; read on with `offset`.",
        schema: || {
            object(
                json!({
                    "path": {"type": "string"},
                    "offset": {"type": "integer", "description": "Byte position to start at. Default 0."},
                    "max_bytes": {"type": "integer", "description": "At most this many bytes, up to 262144."}
                }),
                &["path"],
            )
        },
        run: pull,
    },
    Tool {
        name: "screenshot",
        description: "A picture of the whole screen, one window or a region, as a PNG. Positions given to click, move, \
                      drag and scroll afterwards are pixels of this picture. `max_width` shrinks it to save space.",
        schema: || {
            object(
                json!({
                    "window": {"description": "A window's id or title: only that window's area."},
                    "region": {"type": "object", "description": "x, y, width, height in desktop pixels.",
                               "properties": {"x": {"type": "number"}, "y": {"type": "number"}, "width": {"type": "number"}, "height": {"type": "number"}}},
                    "max_width": {"type": "integer", "description": "Shrink the picture to at most this many pixels wide."},
                    "save_to": {"type": "string", "description": "Write the PNG to this path on the machine instead of returning it."}
                }),
                &[],
            )
        },
        run: crate::control::screenshot,
    },
    Tool {
        name: "windows",
        description: "The open windows, frontmost first: id, title, app, pid, place and size in desktop pixels, which \
                      one has the focus, which are minimized. Also the size of the whole screen.",
        schema: || object(json!({}), &[]),
        run: crate::control::windows,
    },
    Tool {
        name: "focus",
        description: "Bring a window to the front. Refused if the window doesn't take the focus.",
        schema: || object(json!({"window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}}), &["window"]),
        run: crate::control::focus,
    },
    Tool {
        name: "click",
        description: "Click at a position. With `window`, that window is brought to the front first and the position \
                      must be inside it. With `background` (macOS) nothing comes to the front and the pointer stays \
                      where it is; `type_text` and `key` take it too.",
        schema: || object(json!({"background": {"type": "boolean", "description": "macOS only: leave the pointer, the real keyboard and the window order alone. A click then presses what is at the point (or gives a text field the keyboard) through assistive technology; text and keys are handed to the window's app only."}, "element": {"type": "string", "description": "An element id from tree or find, instead of x and y: the action goes to its centre and its window is brought to the front."}, "x": {"type": "number", "description": "In the pixels of the last screenshot (or of `capture`)."}, "y": {"type": "number"}, "capture": {"type": "integer", "description": "The screenshot the position is from. Default: the last one."}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}, "button": {"type": "string", "enum": ["left", "middle", "right"]}}), &[]),
        run: crate::control::click,
    },
    Tool {
        name: "double_click",
        description: "Double-click at a position.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find, instead of x and y: the action goes to its centre and its window is brought to the front."}, "x": {"type": "number", "description": "In the pixels of the last screenshot (or of `capture`)."}, "y": {"type": "number"}, "capture": {"type": "integer", "description": "The screenshot the position is from. Default: the last one."}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}}), &[]),
        run: crate::control::double_click,
    },
    Tool {
        name: "right_click",
        description: "Click the right button at a position.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find, instead of x and y: the action goes to its centre and its window is brought to the front."}, "x": {"type": "number", "description": "In the pixels of the last screenshot (or of `capture`)."}, "y": {"type": "number"}, "capture": {"type": "integer", "description": "The screenshot the position is from. Default: the last one."}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}}), &[]),
        run: crate::control::right_click,
    },
    Tool {
        name: "move",
        description: "Move the pointer to a position without clicking.",
        schema: || object(json!({"x": {"type": "number", "description": "In the pixels of the last screenshot (or of `capture`)."}, "y": {"type": "number"}, "capture": {"type": "integer", "description": "The screenshot the position is from. Default: the last one."}}), &["x", "y"]),
        run: crate::control::move_pointer,
    },
    Tool {
        name: "drag",
        description: "Press a button at one position, move to another, and release.",
        schema: || {
            object(
                json!({
                    "from_x": {"type": "number"}, "from_y": {"type": "number"}, "to_x": {"type": "number"}, "to_y": {"type": "number"},
                    "capture": {"type": "integer"}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}, "button": {"type": "string", "enum": ["left", "middle", "right"]}
                }),
                &["from_x", "from_y", "to_x", "to_y"],
            )
        },
        run: crate::control::drag,
    },
    Tool {
        name: "scroll",
        description: "Turn the mouse wheel at a position, in notches: `down` (negative is up), `right` (negative is left).",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find, instead of x and y: the action goes to its centre and its window is brought to the front."}, "x": {"type": "number", "description": "In the pixels of the last screenshot (or of `capture`)."}, "y": {"type": "number"}, "capture": {"type": "integer", "description": "The screenshot the position is from. Default: the last one."}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}, "down": {"type": "integer"}, "right": {"type": "integer"}}), &[]),
        run: crate::control::scroll,
    },
    Tool {
        name: "type_text",
        description: "Type text into a window, whatever the keyboard layout. Give `window`, or an `element` to type \
                      into; the window must take the focus, or nothing is typed. A line break presses Enter. The answer's \
                      `into` names what had the keyboard, where the system can tell: sent is not the same as landed \
                      where you meant.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find to give the keyboard to first, so the keys go into it and not into whatever in the window had the keyboard. The window is then the element's own."}, "background": {"type": "boolean", "description": "macOS only: leave the pointer, the real keyboard and the window order alone. A click then presses what is at the point (or gives a text field the keyboard) through assistive technology; text and keys are handed to the window's app only."}, "text": {"type": "string"}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}}), &["text"]),
        run: crate::control::type_text,
    },
    Tool {
        name: "key",
        description: "Press a key or a chord in a window: enter, tab, escape, f5, ctrl+s, ctrl+shift+t, alt+f4, \
                      win+ctrl+right. The window is required and must take the focus, or nothing is pressed.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find to give the keyboard to first, so the keys go into it and not into whatever in the window had the keyboard. The window is then the element's own."}, "background": {"type": "boolean", "description": "macOS only: leave the pointer, the real keyboard and the window order alone. A click then presses what is at the point (or gives a text field the keyboard) through assistive technology; text and keys are handed to the window's app only."}, "keys": {"type": "string"}, "window": {"description": "The window it is for: its id or its title (or a part of the title only one window has). It is brought to the front first; the action is refused if it doesn't come."}, "times": {"type": "integer", "description": "Press it this many times. Default 1."}}), &["keys"]),
        run: crate::control::key,
    },
    Tool {
        name: "clipboard_get",
        description: "The text on the clipboard; null when it is empty or holds something that isn't text.",
        schema: || object(json!({}), &[]),
        run: crate::control::clipboard_get,
    },
    Tool {
        name: "clipboard_set",
        description: "Put text on the clipboard, replacing what is there.",
        schema: || object(json!({"text": {"type": "string"}}), &["text"]),
        run: crate::control::clipboard_set,
    },
    Tool {
        name: "tree",
        description: "What is in a window, as assistive technology sees it: every element with an id, its depth, role, \
                      name, value and place in desktop pixels. `enabled` is given only when false and `focused` only \
                      when true. An id can be passed as `element` to click, double_click, right_click and scroll.",
        schema: || object(json!({"window": {"description": "The window: its id or its title (or a part of the title only one window has)."}, "max_depth": {"type": "integer"}, "max_elements": {"type": "integer", "description": "Stop after this many elements. Default 1000."}}), &["window"]),
        run: crate::control::tree,
    },
    Tool {
        name: "find",
        description: "The elements of a window that match a role, a name and/or a value.",
        schema: || object(json!({"window": {"description": "The window: its id or its title (or a part of the title only one window has)."}, "role": {"type": "string", "description": "Exactly this role: button, text, label, check_box, menu_item, list_item, tab..."}, "name": {"type": "string", "description": "The name contains this, in any case."}, "value": {"type": "string", "description": "The value contains this, in any case."}, "max_depth": {"type": "integer"}, "max_elements": {"type": "integer", "description": "Stop after this many elements. Default 1000."}}), &["window"]),
        run: crate::control::find,
    },
    Tool {
        name: "wait_for",
        description: "Wait until a window exists, or until an element matching role, name and/or value is in it. With \
                      `gone`, wait until it no longer is. Answers `timed_out` if that doesn't happen in time.",
        schema: || {
            object(
                json!({"window": {"description": "The window: its id or its title (or a part of the title only one window has)."}, "role": {"type": "string", "description": "Exactly this role: button, text, label, check_box, menu_item, list_item, tab..."}, "name": {"type": "string", "description": "The name contains this, in any case."}, "value": {"type": "string", "description": "The value contains this, in any case."}, "max_depth": {"type": "integer"}, "max_elements": {"type": "integer", "description": "Stop after this many elements. Default 1000."}, "gone": {"type": "boolean"}, "timeout_s": {"type": "integer", "description": "Default 10, at most 120."}}),
                &["window"],
            )
        },
        run: crate::control::wait_for,
    },
    Tool {
        name: "press",
        description: "Do what a click on an element would do (press a button, follow a link, choose a menu item), by its \
                      id from tree or find. Like the other element tools it works through assistive technology: the \
                      pointer and the keyboard are left alone and the window needn't be in front. Answers \
                      `not_supported` when the element can't do it, `element_gone` when it is no longer there.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find, like 3.17."}}), &["element"]),
        run: crate::control::press_element,
    },
    Tool {
        name: "set_value",
        description: "Set what an element holds: the text of a field, the number of a slider or spinner. Replaces what \
                      was there. Answers with the value it holds afterwards.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find."}, "value": {"description": "The text or the number to set."}}), &["element", "value"]),
        run: crate::control::set_value,
    },
    Tool {
        name: "toggle",
        description: "Tick or untick a check box, switch or toggle button. With `on` it is left alone when it is already \
                      that way; without, it is flipped. Answers with `on` as it is afterwards.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find."}, "on": {"type": "boolean", "description": "The state wanted. Leave out to flip it."}}), &["element"]),
        run: crate::control::toggle,
    },
    Tool {
        name: "expand",
        description: "Open or close something that unfolds: a tree item, an expander, a combo box's list, a menu.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find."}, "expanded": {"type": "boolean", "description": "true opens it (the default), false closes it."}}), &["element"]),
        run: crate::control::expand,
    },
    Tool {
        name: "select",
        description: "Select an item among its siblings: a list item, a tab, a radio button, a combo box's option.",
        schema: || object(json!({"element": {"type": "string", "description": "An element id from tree or find."}}), &["element"]),
        run: crate::control::select,
    },
    Tool {
        name: "observe_start",
        description: "Start a command, or follow one that is running (`pid`), and record what it and everything it starts do: programs run, files opened, read, \
                      written, renamed and deleted, network connections, memory mappings, crashes (what is told differs by \
                      system: on macOS there are no byte counts, addresses or system calls). Give `argv` or \
                      `command` as for exec. `allow` lists the only folders it may change and `guard` the files and \
                      folders it must not; breaking either is a problem in the verdict. Returns a session number.",
        schema: || {
            object(
                json!({
                    "argv": {"type": "array", "items": {"type": "string"}},
                    "command": {"type": "string", "description": "A shell command line, instead of argv."},
                    "pid": {"type": "integer", "description": "Instead of argv or command: follow this running program, and what it has started and starts, from now on. Stopping the session leaves it running. Needs admin rights on Linux (unless the kernel allows tracing other programs) and on macOS."},
                    "stacks": {"type": "boolean", "description": "Give every event the call stack it was made from, innermost frame first, as file!function+offset. Costs time on every event. Linux and Windows; on Linux a crash carries its stack either way."},
                    "classes": {"type": "array", "items": {"type": "string"}, "description": "Keep only these classes of events (process, file, registry, network, memory, ipc, syscall...). Problems are always kept."},
                    "include": {"type": "array", "items": {"type": "string"}, "description": "Keep file and registry events only under these paths."},
                    "exclude": {"type": "array", "items": {"type": "string"}, "description": "Drop file and registry events under these paths (system libraries, caches)."},
                    "cwd": {"type": "string"},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}},
                    "syscalls": {"type": "string", "enum": ["off", "full"], "description": "full also records every system call as an event. Default off."},
                    "allow": {"type": "array", "items": {"type": "string"}, "description": "Folders the target may change. Empty: no limit."},
                    "guard": {"type": "array", "items": {"type": "string"}, "description": "Files and folders that must not change."},
                    "self_sandboxed": {"type": "boolean", "description": "macOS only: the target sets up a sandbox of its own (a browser, an App Sandbox app). It is then watched from outside only, which needs admin rights and records no network connections."}
                }),
                &[],
            )
        },
        run: crate::observe::observe_start,
    },
    Tool {
        name: "observe_stop",
        description: "End a session: stop the target if it still runs, and return the verdict (ok or problem), the \
                      problems found and a summary. Can be asked again later for the same answer.",
        schema: || object(json!({"session": {"type": "integer"}}), &["session"]),
        run: crate::observe::observe_stop,
    },
    Tool {
        name: "events",
        description: "Search a session's events, oldest first. Filter by class (process, file, network, memory, syscall, \
                      problem...), pid, op (open, read, write, delete, rename, connect, exec, exit...), part of the \
                      subject (a path or an address) or any text. Paged: pass next_offset back as offset.",
        schema: || {
            object(
                json!({
                    "session": {"type": "integer"},
                    "class": {"description": "One class or a list of them."},
                    "pid": {"type": "integer"},
                    "op": {"type": "string"},
                    "subject": {"type": "string", "description": "The subject contains this, in any case."},
                    "text": {"type": "string", "description": "The subject, operation, program name, result or arguments contain this."},
                    "from_ns": {"type": "integer"}, "until_ns": {"type": "integer"},
                    "limit": {"type": "integer", "description": "Default 200, at most 2000."},
                    "offset": {"type": "integer"}
                }),
                &["session"],
            )
        },
        run: crate::observe::events,
    },
    Tool {
        name: "summary",
        description: "A session so far: its state, events per class, programs run, files changed and read, network \
                      addresses, each with how many events.",
        schema: || object(json!({"session": {"type": "integer"}}), &["session"]),
        run: crate::observe::summary,
    },
    Tool {
        name: "logs",
        description: "What the session's target printed: its stdout or stderr from a position, 256 KB a call.",
        schema: || object(json!({"session": {"type": "integer"}, "stream": {"type": "string", "enum": ["stdout", "stderr"]}, "from": {"type": "integer"}}), &["session"]),
        run: crate::observe::logs,
    },
    Tool {
        name: "sessions",
        description: "Every observation session since the agent started, with its state.",
        schema: || object(json!({}), &[]),
        run: crate::observe::sessions,
    },
    Tool {
        name: "batch",
        description: "Run several tools one after another in one call, to save round trips: find a field, set it, press \
                      a button, wait for what follows. Each step is `{tool, args}`. Where a step needs something an \
                      earlier step answered (an element id, a session number), give `{\"from_step\": 1, \"path\": \
                      \"elements.0.id\"}` in place of the value; steps count from 1. It stops at the first step that \
                      is refused, unless `keep_going`. The answer lists each step's result or error.",
        schema: || {
            object(
                json!({
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 30,
                        "items": {"type": "object", "properties": {"tool": {"type": "string"}, "args": {"type": "object"}}, "required": ["tool"], "additionalProperties": false}
                    },
                    "keep_going": {"type": "boolean", "description": "Run the remaining steps after one is refused. Default false."}
                }),
                &["steps"],
            )
        },
        run: batch,
    },
];

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

pub fn call(agent: &Agent, name: &str, args: &Value) -> Result<Value, ToolError> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| ToolError { code: "unknown_tool", message: format!("there is no tool named {name}") })?;
    let empty = json!({});
    let args = if args.is_null() { &empty } else { args };
    let given = args.as_object().ok_or_else(|| ToolError::bad("the arguments must be a JSON object"))?;
    let known = (tool.schema)();
    if let Some(unknown) = given.keys().find(|key| known["properties"].get(key.as_str()).is_none()) {
        return Err(ToolError::bad(format!("{name} has no argument named {unknown}")));
    }
    (tool.run)(agent, args)
}

// ------------------------------------------------------------------------------------------------ batch

/// A step's arguments with every `{from_step, path}` replaced by what that earlier step answered there.
fn fill_in(value: &Value, answers: &[Option<Value>]) -> Result<Value, ToolError> {
    match value {
        Value::Object(fields) if fields.len() == 2 && fields.contains_key("from_step") && fields.contains_key("path") => {
            let number = fields["from_step"].as_u64().filter(|number| *number >= 1 && *number as usize <= answers.len());
            let number = number.ok_or_else(|| ToolError::bad(format!("from_step must be the number of an earlier step (1 to {})", answers.len())))? as usize;
            let path = fields["path"].as_str().ok_or_else(|| ToolError::bad("path must be text, like elements.0.id"))?;
            let answer = answers[number - 1].as_ref().ok_or_else(|| ToolError::bad(format!("step {number} was refused, so there is nothing to take from it")))?;
            let found = path.split('.').filter(|part| !part.is_empty()).try_fold(answer, |at, part| match at {
                Value::Array(items) => part.parse::<usize>().ok().and_then(|index| items.get(index)),
                other => other.get(part),
            });
            found.filter(|found| !found.is_null()).cloned().ok_or_else(|| ToolError::bad(format!("step {number}'s answer has nothing at {path}")))
        }
        Value::Object(fields) => Ok(Value::Object(fields.iter().map(|(name, value)| Ok((name.clone(), fill_in(value, answers)?))).collect::<Result<_, ToolError>>()?)),
        Value::Array(items) => Ok(Value::Array(items.iter().map(|item| fill_in(item, answers)).collect::<Result<_, _>>()?)),
        other => Ok(other.clone()),
    }
}

fn batch(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let steps = args["steps"].as_array().filter(|steps| !steps.is_empty() && steps.len() <= 30).ok_or_else(|| ToolError::bad("steps is required: 1 to 30 steps, each {tool, args}"))?;
    let keep_going = flag(args, "keep_going")?;
    // The whole list is checked before anything runs: a slip in step 5 must not leave steps 1 to 4 done.
    for (index, step) in steps.iter().enumerate() {
        let number = index + 1;
        let fields = step.as_object().ok_or_else(|| ToolError::bad(format!("step {number} must be an object: {{tool, args}}")))?;
        let tool = fields.get("tool").and_then(Value::as_str).ok_or_else(|| ToolError::bad(format!("step {number}: tool is required")))?;
        if tool == "batch" {
            return Err(ToolError::bad(format!("step {number}: a batch can't hold a batch")));
        }
        if !TOOLS.iter().any(|known| known.name == tool) {
            return Err(ToolError::bad(format!("step {number}: there is no tool named {tool}")));
        }
        if let Some(unknown) = fields.keys().find(|name| !matches!(name.as_str(), "tool" | "args")) {
            return Err(ToolError::bad(format!("step {number} has {unknown}; a step is {{tool, args}}")));
        }
        if !matches!(fields.get("args"), None | Some(Value::Null | Value::Object(_))) {
            return Err(ToolError::bad(format!("step {number}: args must be an object")));
        }
    }
    let mut answers: Vec<Option<Value>> = Vec::new();
    let mut results = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let tool = step["tool"].as_str().expect("checked");
        let outcome = fill_in(&step["args"], &answers).and_then(|args| agent.call("batch", tool, &args));
        match outcome {
            Ok(answer) => {
                results.push(json!({ "step": index + 1, "tool": tool, "ok": true, "result": answer }));
                answers.push(Some(answer));
            }
            Err(error) => {
                results.push(json!({ "step": index + 1, "tool": tool, "ok": false, "error": { "code": error.code, "message": error.message } }));
                answers.push(None);
                if !keep_going {
                    break;
                }
            }
        }
    }
    let refused = answers.iter().filter(|answer| answer.is_none()).count();
    Ok(json!({ "ok": refused == 0, "done": answers.len() - refused, "refused": refused, "not_run": steps.len() - answers.len(), "steps": results }))
}

// ------------------------------------------------------------------------------------------------ arguments

pub(crate) fn text<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    match &args[name] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(ToolError::bad(format!("{name} must be text"))),
    }
}

pub(crate) fn required_text<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    text(args, name)?.filter(|text| !text.is_empty()).ok_or_else(|| ToolError::bad(format!("{name} is required")))
}

pub(crate) fn number(args: &Value, name: &str) -> Result<Option<u64>, ToolError> {
    match &args[name] {
        Value::Null => Ok(None),
        value => value.as_u64().map(Some).ok_or_else(|| ToolError::bad(format!("{name} must be a whole number, zero or more"))),
    }
}

pub(crate) fn flag(args: &Value, name: &str) -> Result<bool, ToolError> {
    match &args[name] {
        Value::Null => Ok(false),
        Value::Bool(value) => Ok(*value),
        _ => Err(ToolError::bad(format!("{name} must be true or false"))),
    }
}

fn find_job(agent: &Agent, args: &Value) -> Result<std::sync::Arc<Job>, ToolError> {
    let id = number(args, "id")?.ok_or_else(|| ToolError::bad("id is required"))?;
    agent.jobs.get(id).ok_or_else(|| ToolError::not_found(format!("there is no job {id}")))
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

// ------------------------------------------------------------------------------------------------ tools

fn status(agent: &Agent, _args: &Value) -> Result<Value, ToolError> {
    let (screen, input, permissions_needed) = crate::desktop::ready();
    Ok(json!({
        "name": "master-control",
        "version": crate::VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": platform::hostname(),
        "identity": crate::identity::this_machine().to_json(),
        "user": platform::user(),
        "pid": std::process::id(),
        "elevated": platform::elevated(),
        "session": platform::session(),
        "uptime_ms": agent.started.elapsed().as_millis() as u64,
        "jobs_running": agent.jobs.running(),
        "tools": TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        // On macOS: what the user has yet to allow in System Settings > Privacy & Security.
        "permissions_needed": permissions_needed,
        "capabilities": {
            "exec": true,
            "files": true,
            "screen": screen,
            "input": input,
            // The tree of a window's elements. On macOS it needs the same permission as input;
            // elsewhere it is there whenever the desktop is.
            "accessibility": if cfg!(target_os = "macos") { input } else { screen },
            "observe": crate::observe::BUILT_IN,
        },
    }))
}

fn job_state(job: &Job) -> Value {
    let snapshot = job.snapshot();
    let state = match snapshot.ended {
        None => "running",
        Some(Ended::Exited) => "exited",
        Some(Ended::Killed) => "killed",
        Some(Ended::TimedOut) => "timed_out",
    };
    json!({
        "id": job.id,
        "pid": job.pid,
        "command": job.command,
        "state": state,
        "exit_code": snapshot.exit_code,
        "started_ms": job.started_ms,
        "ended_ms": snapshot.ended_ms,
    })
}

fn exec(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let argv: Vec<String> = match &args["argv"] {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string).ok_or_else(|| ToolError::bad("argv must be a list of text")))
            .collect::<Result<_, _>>()?,
        _ => return Err(ToolError::bad("argv must be a list of text")),
    };
    let command = text(args, "command")?.unwrap_or_default().to_string();
    match (argv.is_empty(), command.is_empty()) {
        (true, true) => return Err(ToolError::bad("give argv or command")),
        (false, false) => return Err(ToolError::bad("give argv or command, not both")),
        _ => {}
    }
    if argv.first().is_some_and(String::is_empty) {
        return Err(ToolError::bad("argv's first item is the program and can't be empty"));
    }
    let env = match &args["env"] {
        Value::Null => Vec::new(),
        Value::Object(variables) => variables
            .iter()
            .map(|(name, value)| {
                let value = value.as_str().ok_or_else(|| ToolError::bad(format!("env.{name} must be text")))?;
                if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
                    return Err(ToolError::bad(format!("env.{name} is not a usable variable")));
                }
                Ok((name.clone(), value.to_string()))
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(ToolError::bad("env must be an object of text values")),
    };
    let cwd = text(args, "cwd")?.map(state::resolve);
    if let Some(folder) = cwd.as_ref().filter(|folder| !folder.is_dir()) {
        return Err(ToolError::not_found(format!("the working folder {} doesn't exist", folder.display())));
    }
    let timeout = number(args, "timeout_s")?.unwrap_or(DEFAULT_TIMEOUT_S);
    if timeout == 0 || timeout > LONGEST_TIMEOUT_S {
        return Err(ToolError::bad(format!("timeout_s must be between 1 and {LONGEST_TIMEOUT_S}; use detach for longer runs")));
    }
    let detach = flag(args, "detach")?;
    let (argv, command) = if flag(args, "elevated")? { elevate(argv, command)? } else { (argv, command) };
    let spec = Spec { argv, command, cwd, env, stdin: text(args, "stdin")?.map(str::to_string) };
    let job = agent.jobs.start(spec).map_err(|error| ToolError::failed(format!("the command couldn't be started: {error}")))?;
    if detach {
        return Ok(json!({ "job": job.id, "pid": job.pid }));
    }
    if !job.wait(Duration::from_secs(timeout)) {
        job.stop(Ended::TimedOut);
        // Stopping is not instant; give the system a moment to report the exit.
        job.wait(Duration::from_secs(10));
    }
    let (stdout, stdout_cut) = job.tail(Stream::Stdout, CHUNK).map_err(read_error)?;
    let (stderr, stderr_cut) = job.tail(Stream::Stderr, CHUNK).map_err(read_error)?;
    let snapshot = job.snapshot();
    Ok(json!({
        "job": job.id,
        "exit_code": snapshot.exit_code,
        "timed_out": snapshot.ended == Some(Ended::TimedOut),
        "stdout": lossy(stdout),
        "stderr": lossy(stderr),
        "stdout_truncated": stdout_cut,
        "stderr_truncated": stderr_cut,
        "took_ms": snapshot.ended_ms.map(|ended| ended.saturating_sub(job.started_ms)),
    }))
}

/// The command as it has to be run to have admin rights, or why it can't have them here.
fn elevate(argv: Vec<String>, command: String) -> Result<(Vec<String>, String), ToolError> {
    if platform::elevated() {
        return Ok((argv, command));
    }
    if cfg!(windows) {
        return Err(ToolError { code: "not_supported", message: "the agent isn't running with admin rights; install it as a worker (mc-agent install) so it is".into() });
    }
    // -n: never ask for a password, which nobody is there to type.
    let allowed = std::process::Command::new("sudo").args(["-n", "true"]).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success());
    if !allowed {
        return Err(ToolError { code: "not_supported", message: "sudo asks this user for a password, so nothing can be run with admin rights; see mc-agent install".into() });
    }
    let run = if argv.is_empty() { vec!["sh".to_string(), "-c".to_string(), command] } else { argv };
    Ok((["sudo", "-n", "--"].map(str::to_string).into_iter().chain(run).collect(), String::new()))
}

fn read_error(error: std::io::Error) -> ToolError {
    ToolError::failed(format!("the job's output couldn't be read: {error}"))
}

fn job(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let job = find_job(agent, args)?;
    if let Some(wait) = number(args, "wait_ms")? {
        job.wait(Duration::from_millis(wait.min(LONGEST_WAIT_MS)));
    }
    // The state first: output read after an exit was seen is the whole output.
    let mut answer = job_state(&job);
    for (stream, name) in [(Stream::Stdout, "stdout"), (Stream::Stderr, "stderr")] {
        let from = number(args, &format!("{name}_from"))?.unwrap_or(0);
        let (bytes, size) = job.read(stream, from, CHUNK).map_err(read_error)?;
        let next = from.min(size) + bytes.len() as u64;
        answer[format!("{name}_next")] = json!(next);
        answer[format!("{name}_more")] = json!(next < size);
        answer[name] = json!(lossy(bytes));
    }
    Ok(answer)
}

fn list_jobs(agent: &Agent, _args: &Value) -> Result<Value, ToolError> {
    Ok(json!({ "jobs": agent.jobs.all().iter().map(|job| job_state(job)).collect::<Vec<_>>() }))
}

fn kill(agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let job = find_job(agent, args)?;
    let stopped = job.stop(Ended::Killed);
    job.wait(Duration::from_secs(10));
    let mut answer = job_state(&job);
    answer["was_running"] = json!(stopped);
    Ok(answer)
}

fn push(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let path = state::resolve(required_text(args, "path")?);
    let bytes = match (text(args, "content_base64")?, text(args, "text")?) {
        (Some(_), Some(_)) => return Err(ToolError::bad("give content_base64 or text, not both")),
        (Some(encoded), None) => BASE64.decode(encoded).map_err(|error| ToolError::bad(format!("content_base64 isn't base64: {error}")))?,
        (None, Some(text)) => text.as_bytes().to_vec(),
        (None, None) => return Err(ToolError::bad("give content_base64 or text")),
    };
    write_file(&path, &bytes)
}

pub fn write_file(path: &Path, bytes: &[u8]) -> Result<Value, ToolError> {
    if path.is_dir() {
        return Err(ToolError::bad(format!("{} is a folder", path.display())));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ToolError::failed(format!("{}: {error}", parent.display())))?;
    }
    jobs::write_whole(path, bytes).map_err(|error| ToolError::failed(format!("{}: {error}", path.display())))?;
    Ok(json!({ "path": path.display().to_string(), "bytes": bytes.len() }))
}

pub fn open_file(path: &Path) -> Result<(fs::File, u64), ToolError> {
    let describe = |error: std::io::Error| {
        let message = format!("{}: {error}", path.display());
        if error.kind() == std::io::ErrorKind::NotFound {
            ToolError::not_found(message)
        } else {
            ToolError::failed(message)
        }
    };
    // Asked first: opening a folder as a file fails on Windows with a message about access, which says nothing.
    if path.is_dir() {
        return Err(ToolError::bad(format!("{} is a folder", path.display())));
    }
    let file = fs::File::open(path).map_err(describe)?;
    let metadata = file.metadata().map_err(describe)?;
    if metadata.is_dir() {
        return Err(ToolError::bad(format!("{} is a folder", path.display())));
    }
    Ok((file, metadata.len()))
}

fn pull(_agent: &Agent, args: &Value) -> Result<Value, ToolError> {
    let path = state::resolve(required_text(args, "path")?);
    let offset = number(args, "offset")?.unwrap_or(0);
    let max = number(args, "max_bytes")?.map_or(CHUNK, |max| (max as usize).min(CHUNK));
    let (mut file, size) = open_file(&path)?;
    let offset = offset.min(size);
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.take(max as u64).read_to_end(&mut bytes))
        .map_err(|error| ToolError::failed(format!("{}: {error}", path.display())))?;
    let next = offset + bytes.len() as u64;
    Ok(json!({
        "path": path.display().to_string(),
        "size": size,
        "offset": offset,
        "next": next,
        "eof": next >= size,
        "content_base64": BASE64.encode(&bytes),
    }))
}
