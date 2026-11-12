# Master Control: a guide for coding agents

You are an AI coding agent, and this machine (or one near it) runs Master Control. This file tells you how to
connect to it and how to use it well. It ships inside the program: `mctl guide` prints it, the MCP tool `guide`
returns it, and Start leaves a copy next to the installed program.

## What it gives you

Master Control lets you work on real machines the way a person at the keyboard would, and see what a program
really does while it runs:

- **Run things:** commands, long jobs, files in and out.
- **See and drive the desktop:** screenshots, windows, mouse, keyboard, clipboard, and what is inside a window
  (buttons, fields, labels) with actions on those elements.
- **Observe a program:** every program it starts, file it reads or changes, connection it makes; crashes; and a
  verdict on rules you set ("it may only write here", "it must not touch that").

One machine is the **master**: the one you run on. Others are **workers** it is paired with. You reach all of
them through the master, by name. The tools are the same on every machine; what each can do differs a little by
system, and `status` tells you.

## Connecting

Use whichever of these your harness supports. All three reach the same tools.

### 1. MCP (best)

Add a stdio MCP server that runs the master program with the one argument `mcp`:

```json
{ "mcpServers": { "master-control": { "command": "/path/to/mctl", "args": ["mcp"] } } }
```

```toml
[mcp_servers.master-control]
command = '/path/to/mctl'
args = ["mcp"]
```

The program is at `~/.local/opt/master-control/mctl` on macOS and Linux, and
`%LOCALAPPDATA%\Programs\Master Control\mctl.exe` on Windows. `mctl agents` prints the exact command for this
machine.

The user can have this done for them: `mctl start` (or Start in the Master Control app) adds the server to every
agent it finds: Claude Code, the Claude app, Codex, Antigravity, Gemini CLI, Cursor, Windsurf, VS Code. An agent
that was open has to be started again to see the tools.

### 2. Command line

```
mctl machines                         # this machine and its workers
mctl call MACHINE TOOL 'JSON'         # one tool, arguments as a JSON object
mctl call linux exec '{"command": "uname -a"}'
```

Each `mctl call` is a new process, so nothing is remembered between calls: no "last screenshot", no element ids,
no observation session. For anything with more than one step, use MCP, or put the steps in one `batch` call.

### 3. A machine's own HTTP API

Each machine also serves its tools to programs on that same machine, at `http://127.0.0.1:47900`:
`POST /v1/<tool>` with a JSON object, and `POST /mcp` for MCP over HTTP. Every request needs
`Authorization: Bearer <token>`; `mc-agent token` prints the token. This reaches that one machine only, and takes
no `machine` argument.

## First steps

1. Call `machines`. It lists the master (named `master`) and each worker, with its system and whether it is
   reachable.
2. Every other tool takes `machine`: a name from that list.
3. Call `status` on a machine before relying on it. `capabilities` says what works there (`screen`, `input`,
   `accessibility`, `observe`), `elevated` whether commands can run with admin rights, and `permissions_needed`
   what the user still has to allow (on macOS: Screen Recording, Accessibility).

## The tools

| Group | Tools | In short |
|---|---|---|
| Machines | `machines`, `status`, `guide` | Who is there, what works, this guide |
| Commands | `exec`, `job`, `jobs`, `kill` | Run a command (`argv` or `command`); `detach` for long ones, then `job` to follow it; `elevated` for admin rights |
| Files | `push`, `pull` | Write a file from text or base64; read one, in parts if it is large |
| Screen | `screenshot`, `windows` | A picture of the desktop, a window or a region; the list of windows |
| Pointer | `click`, `double_click`, `right_click`, `move`, `drag`, `scroll` | At a position, or at an element by its id |
| Keyboard | `type_text`, `key`, `focus` | Text and key chords into a named window |
| Clipboard | `clipboard_get`, `clipboard_set` | The session's clipboard, text only |
| Inside a window | `tree`, `find`, `wait_for` | Elements with role, name, value and place; wait for one to appear or go |
| Element actions | `press`, `set_value`, `toggle`, `expand`, `select` | Act on an element itself: no pointer, no keyboard, the window need not be in front |
| Observation | `observe_start`, `observe_stop`, `events`, `summary`, `logs`, `sessions` | Record what a program does, search it, get a verdict |
| Several at once | `batch` | Up to 30 steps in one call, a later step using an earlier one's answer |

The MCP tool list has the full description and arguments of each.

## Rules that will save you trouble

**Positions belong to a picture.** `x` and `y` are in the pixels of the last `screenshot` you took (or of the one
you name with `capture`). Take a fresh screenshot after anything that could have changed the screen.

**Keys go to a named window, or better, to a named element.** `type_text` and `key` need `window` (its id or
title) or `element` (an id from `find`). With `element`, that element is given the keyboard first, so the keys go
into it and not into whatever in the window had the keyboard (a browser's address bar, say). The window must
come to the front, or nothing is sent and you get `focus_refused`, with the name of what is in front instead.

**Sent is not landed.** The answer's `into` (macOS, Windows) names what had the keyboard: check it is what you
meant, and check the result (`find` the field, or take a screenshot) before going on.

**Prefer elements to positions.** `find` a button by role and name, then `press` it. That survives a moved
window and a different screen size, and leaves the user's pointer alone. Use positions for what has no element
(a canvas, a game, a drawing).

**System dialogs are windows too (macOS).** A dialog the system shows for an app (a permission prompt such as
"Allow X to access your contacts?", an alert) is in `windows` under the app `UserNotificationCenter`, titled with
its question. `find` its buttons and `press` one: the pointer isn't used. Answer one only when the user's task
calls for it.

**Element ids are short-lived.** An id like `3.17` belongs to the tree it came from. If the window changed, the
id is refused as `element_gone`: call `find` again and use the new id. Never keep ids across steps that change
the window.

**Leave the user's desktop alone when you can.** On macOS, `click`, `type_text` and `key` take
`background: true`: nothing comes to the front and the real pointer and keyboard are not used. Element actions
never use them, on any system. A person may be working on that machine.

**Use `batch` for sequences.** It saves round trips and keeps steps together:

```json
{ "machine": "windows", "steps": [
  { "tool": "find", "args": { "window": "Settings", "role": "button", "name": "Save" } },
  { "tool": "press", "args": { "element": { "from_step": 1, "path": "elements.0.id" } } },
  { "tool": "wait_for", "args": { "window": "Settings", "role": "label", "name": "Saved", "timeout_s": 10 } }
] }
```

`{"from_step": N, "path": "a.0.b"}` in place of a value takes it from step N's answer. The batch stops at the
first step that is refused unless `keep_going` is set. Check `ok` and each step's `ok`.

**Refusals have codes.** A tool that can't do what you asked answers with a code and a sentence that says why:

| Code | Meaning | What to do |
|---|---|---|
| `bad_input` | An argument is missing, wrong or unknown | Fix the call |
| `not_found`, `window_not_found`, `unknown_tool` | No such file, job, window or tool | Look again (`windows`, `jobs`) |
| `element_gone` | The element is no longer what the tree showed | `find` again |
| `focus_refused` | The window didn't take the keyboard | Something is in the way (a dialog, another app); look with `screenshot` |
| `timed_out` | `wait_for` or a command ran out of time | Decide whether to wait longer |
| `not_supported` | This machine or this element can't do that | Read the sentence; it usually names another way |
| `failed` | It was tried and didn't work | Read the sentence |

Do not retry the same call unchanged after a refusal: read the reason first.

## Observing a program

```json
{ "machine": "linux", "command": "./my-app --self-test", "cwd": "~/project",
  "allow": ["~/project/out"], "guard": ["~/project/src"] }
```

1. `observe_start` starts the command (or, with `pid`, follows a program that is already running) and returns a
   `session` number.
2. Poll `summary` until `state` is no longer `running`, or do your own work on the app meanwhile.
3. `observe_stop` ends the session and returns the **verdict**: `ok`, or `problem` with a list: a crash, a
   write outside the `allow` folders, a change to something in `guard`.
4. `events` searches what was recorded (by `class`, `op`, `subject`, `pid`, `text`); `logs` gives the program's
   output.

Useful options: `classes`, `include` and `exclude` keep the record small; `stacks: true` adds the call stack to
every event (Linux and Windows; on Linux a crash always has one). Stopping a session you started ends the program; stopping one
on a `pid` you attached to leaves it running.

What is recorded differs by system: Linux has system calls, byte counts and addresses; Windows adds the
registry; macOS has programs, files, ports and services, with less detail. `status` and the tool descriptions
say what a machine can do.

## Recipes

**Run a build on another machine and get the result.**
`exec` with `detach: true` → `job` with `wait_ms` until `state` is `exited` → read `exit_code` and the output.

**Copy a file to a worker and run it.**
`push` (`path`, `content_base64`) → `exec`.

**Check a GUI change.**
`exec` to start the app (detached) → `wait_for` its window → `tree` to read it → element actions → `screenshot`
of the window to look at the result.

**Find out why a program misbehaves.**
`observe_start` with `guard` on what it must not touch → reproduce → `observe_stop` → `events` with
`class: "file"` or `"network"` around the time it went wrong.

## Manners

- These are real machines, often with a person using them. Take screenshots of the window you work in rather
  than of the whole desktop when you can, and don't move the pointer or type without a reason.
- Don't put anything on the clipboard the user didn't ask for: it is theirs.
- `elevated` commands run as an administrator. Deleting, installing and changing system settings are the user's
  decisions: ask first.
- Every call is written to the machine's own audit log (without file contents or typed text).

## Versions

`status` gives `version`. Tools are only ever added; when one you expect is missing, the machine runs an older
version, and the user can update it.
