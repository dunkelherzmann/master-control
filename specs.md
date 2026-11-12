# Master Control: specification

Master Control lets a coding agent work a computer the way a person at its desk would, and watch everything an app
under test does while it runs. It is for coding, testing and QA of apps written with agentic coders such as Claude
Code. "Master Control" is a working name.

Status: in development. Built and tested so far: the agent's core (§5) with `status`, `exec`, `job`, `jobs`, `kill`,
`push` and `pull` on macOS, Linux and Windows; the event store (§4.2); and on Linux (X11) and Windows the screen,
input and window-content tools `screenshot`, `windows`, `focus`, `click`, `double_click`, `right_click`, `move`,
`drag`, `scroll`, `type_text`, `key`, `clipboard_get`, `clipboard_set`, `tree`, `find`, `wait_for` and the element
actions `press`, `set_value`, `toggle`, `expand` and `select` (§3); and on
Linux (x86-64) and Windows observation of a started command with `observe_start`, `observe_stop`, `events`,
`summary`, `logs` and `sessions` (§4): process, file, network and memory events, crashes, allowed folders and
guards; on Linux also system-call events, on Windows registry events. On macOS the same screen, input and
window-content tools and observation are built too, with less detail in the events (§4.3).
Where a tool isn't built for a system yet it answers "not supported". Everything else is design.
[pending.md](pending.md) has the build order and the open decisions; points that need a probe before they can be
promised are marked **to prove**.

## 1. What it does

1. **Control.** The agent sees the screen, reads what is on it, and uses the keyboard and mouse (or touch) as a user
   would. It can also run commands, copy files, install and start apps.
2. **Observation.** The agent names a target app, and Master Control records what that app and its child processes
   do, in the foreground and the background: system calls, file reads and writes, registry and settings changes,
   memory activity, network connections in and out, logs, windows and dialogs, crashes and hangs. The agent can
   follow this live, search it afterwards, and get a verdict at the end of a run.
3. **One interface** for both, the same on every platform: an MCP server, an HTTP API and a command-line tool.

Machines it works on: Linux, Windows, macOS and Android; real hardware and virtual machines.

## 2. Parts

| Part | Runs on | What it is |
|---|---|---|
| **Agent** | the machine under test | One self-contained program per platform, running in the signed-in desktop session. Does the capture, input, tracing and storage. Serves the HTTP API and MCP on the loopback interface, protected by a token. A small elevated helper does the work that needs admin or root (kernel tracing, packet capture). |
| **Master** (`mctl`) | the one machine the AI agent runs on and is signed in to | Command-line tool and stdio MCP server. Has full control of itself and of its workers: it finds workers on the local network, pairs with them, and gives the AI agent one tool set with a `machine` argument, where `master` is the master's own machine and the other names are workers. Nothing is signed in on the workers. |
| **Worker** | every other machine | The agent started with `--worker`: it listens on the network for its one master and announces itself every two seconds. Its master has full control of it; nobody else has any. |
| **Android bridge** | the controller or any machine with the phone attached | Drives a phone or emulator over adb. Nothing is installed on the device: a small server is pushed and started through the shell for each session. |
| **Agentless back ends** | the controller or the VM host | For machines that can't run an agent (an installer screen, a crashed OS, a locked-down device): a VM's display and input through its hypervisor, or a physical machine through a KVM-over-IP device. Control only, plus whatever the hypervisor can observe. |

The agent on a machine can also be used directly by a coding agent running on that same machine, with no controller.

## 3. Control

### 3.1 Tools

Names are the same in MCP, HTTP (`POST /v1/<tool>`) and the CLI (`mctl <tool>`).

| Tool | What it does |
|---|---|
| `status` | Agent version, platform, session type, whether the helper is elevated, which capabilities work here |
| `screenshot` | The whole screen, one display, one window or a region; scaled on request; returns an image and a capture id |
| `windows`, `apps` | Open windows (title, app, pid, frame, stacking order, state); installed and running apps. On macOS the dialogs the system shows for an app (a permission prompt, an alert) are listed too, titled with their question, so the element tools can read and answer them |
| `tree` | One window's accessibility tree: role, name, value, frame and state of each element, with element ids tied to that capture |
| `find`, `wait_for` | Find elements by role or text; wait until an element, window or text appears or goes |
| `press`, `set_value`, `toggle`, `expand`, `select` | Act on one element by its id, through assistive technology: the pointer and the keyboard are left alone and the window needn't be in front. Press does what a click would; set_value replaces a field's text or a slider's number; toggle ticks or unticks (to a wanted state, or flipped); expand opens or closes what unfolds; select picks an item among its siblings. The element is found again by the way the tree led to it and must still have the role and name it had (`element_gone` otherwise); `not_supported` when that kind of element can't do it. Built on UI Automation's control patterns (Windows), AT-SPI's actions, editable text, value and selection (Linux) and AX actions and attributes (macOS). Known limit: GTK 4.14's check boxes offer assistive technology nothing to tick them with; the refusal says to click instead |
| `batch` | Several tools one after another in one call, to save round trips. Each step is `{tool, args}`; a value may be taken from an earlier step's answer with `{from_step, path}` (an element id a `find` in the same batch answered, a session number). The list is checked whole before anything runs; it stops at the first refused step unless `keep_going`; the answer lists each step's result or error. Up to 30 steps, no batch inside a batch. Through the master a batch is one trip to the worker. Each step is written to the audit log on its own |
| `type_text`, `key` with `element` | The element is given the keyboard first (through assistive technology; where a toolkit offers no way, by a click on it), so the keys go into it rather than into whatever in the window had the keyboard. The answer carries `into`: the role and name of what had the keyboard, on macOS and Windows |
| `click`, `type_text`, `key` with `background` (macOS) | The same three tools, leaving the pointer, the real keyboard and the window order alone. A click presses what is at the point (it, or the nearest thing around it that can be pressed) or gives a text field the keyboard, through AX. Text and keys are key events handed to the window's app only (`CGEventPostToPid`); the window is first made the app's own main window, which does not bring the app forward. A shortcut with Command chooses the menu item that has it, because the key events themselves reach a background app only some of the time. Double, right and middle clicks, drags and scrolling refuse `background`. Other systems answer `not_supported` |
| `click`, `double_click`, `right_click`, `move`, `drag`, `scroll` | Pointer actions at screen coordinates or on an element id |
| `type_text`, `key` | Text and key chords, sent only once the named target window has the focus |
| `touch` (Android) | Tap, long press, swipe, pinch; the hardware buttons |
| `focus`, `move_window`, `close_window` | Window management |
| `clipboard_get`, `clipboard_set` | Clipboard, saved and restored around a test on request |
| `exec`, `job`, `jobs`, `kill` | Run a command (optionally elevated), follow a long one, stop it |
| `push`, `pull` | Copy files to and from the machine |
| `install`, `launch`, `stop` | Install, start and stop an app or package |
| `record_start`, `record_stop` | Video of the screen or one window during a test |

### 3.2 Rules for input

- Coordinates are always the pixels of the screenshot the agent last received; the agent maps scaled pictures and
  pictures of one window or region back to the desktop. Every screenshot has a `capture` number, and an action can
  name an earlier one.
- Keys and text must name their target window (its id, its title, or a part of the title only one window has) and
  are refused unless that window got the focus, so they never land in whatever happens to be in front. A pointer
  action may name a window; it is then brought to the front first, and a position outside it is refused.
- `type_text` types the characters themselves, whatever the keyboard layout has keys for. `key` presses keys and
  chords by name (`enter`, `f5`, `ctrl+shift+t`) and always releases what it pressed.
- Every action answers with one of: **delivered**, **refused** with an exact reason (`window_not_found`,
  `focus_refused`, `bad_input` for a point off the screen or outside the window; later also blocked by a
  higher-privilege window, element gone), or `not_supported` here. It never reports success it can't confirm: a
  pointer that didn't arrive where it was sent is an error.
- `tree` lists a window's elements, parents before children, each with an id, its depth, role, name, value and
  place. Roles use the same words on every platform where there is one (`button`, `text`, `label`, `check_box`,
  `menu_item`, `tab`…). To keep the list short, `enabled` is given only when false and `focused` only when true.
  `find` returns the elements matching a role, a name or a value; `wait_for` waits for a window, or for a matching
  element in it, to appear or go, and answers `timed_out` otherwise.
- An element id is valid only for the tree it came from. `click`, `double_click`, `right_click` and `scroll` take
  an `element` instead of a position and go to its centre, bringing its window to the front; if the window has
  closed, moved or changed size since, the action is refused with `element_gone` and the agent must look again.
- A window whose app doesn't describe its contents answers `not_supported` to `tree`; pictures and positions
  still work on it.
- `clipboard_set` reads the clipboard back to confirm; `clipboard_get` gives null when it holds no text.

### 3.3 How each platform does it

| | Screen | Input | Reading the UI |
|---|---|---|---|
| **Linux, X11** | X11 capture (GetImage) | XTEST; characters the layout lacks through a borrowed keycode | AT-SPI over the accessibility bus; the agent switches on the session's "assistive technology is listening" flag, which makes Qt and Chromium apps describe themselves |
| **Linux, Wayland** | compositor capture (portal / screencopy), per compositor; **to prove** on GNOME without a prompt on every run | libei or a virtual input device | AT-SPI |
| **Windows** | GDI today; Windows.Graphics.Capture for windows that GDI can't see | SendInput in the desktop session, text as Unicode | UI Automation |
| **macOS** | the system's own `screencapture`, for the rectangle asked for | Core Graphics events; text as the characters themselves; a second click soon after the first is sent as a double click | Accessibility (AX), for the window with the same title among its app's windows. Windows come from the window server's list |
| **Android** | pushed server: encoded screen stream or `screencap` | pushed server: input injection, or `input` commands as fallback | `uiautomator` hierarchy |
| **VM, agentless** | the hypervisor's display (VNC or its own API) | the hypervisor's input channel | none (pictures only) |
| **Physical, agentless** | HDMI capture on a KVM-over-IP device | USB HID gadget keyboard and mouse | none (pictures only) |

Permissions the user grants once: on macOS Screen Recording (pictures, and other apps' window titles) and
Accessibility (input, raising windows) for the agent, in System Settings; until then the tools that need one say
which, and `status` lists what is still needed. On macOS positions are in points, and a picture of a dense
display has more pixels than points, which the tools account for; on Windows the agent
starts at sign-in with highest privileges; on Linux the helper gets root; on Android USB debugging.

## 4. Observation

### 4.1 Sessions

`observe_start` names a target: a command to run, a running pid, an app or package name. The session follows the
target's whole process tree. `observe_stop` ends it and returns the verdict. Several sessions can run at once.
Built: a command the session starts, or a running program by its pid; by app or package name is not built yet.

Each session has a folder in the agent's own: the event store, the target's output, and the tracer's log. A
separate tracer process follows the target, so the agent keeps answering while it runs, and a target the session
started dies with the tracer if the tracer is ever killed (one that was already running is let go instead).

| Tool | What it does |
|---|---|
| `observe_start`, `observe_stop` | Begin and end a session; options choose event classes, guards, allowed folders, a baseline. Built: `pid` follows a program that is already running (it, what it has started and what it starts from then on), and stopping such a session leaves the program running; `classes` keeps only those classes of events, `include` keeps file and registry events only under the paths given and `exclude` drops those under them. The filters thin out what is stored, not what is checked: guards, allowed folders and crashes are judged on everything. Following a running program: on Linux every thread is taken over with ptrace, through sudo where the kernel lets a program be traced only by its parent (`kernel.yama.ptrace_scope`), the files still belonging to the user; on Windows its process ids are added to the trace session's set; on macOS it needs the system's events (admin rights) and records no network. A session in progress on a running program starts with `attach` events in place of `start` |
| `events` | Search a session's events by time, class, process, path, address, text; paged |
| `stream` | Follow events live (HTTP server-sent events; MCP notifications) |
| `summary` | Totals and highlights: files touched, keys written, hosts contacted, peak memory, errors logged, problems |
| `snapshot` | The target's state now: processes, threads, open files, sockets, loaded modules, memory map |
| `stack`, `dump` | A thread's call stack; a memory dump of the process or one region |
| `watch_memory` | Report every read or write of one address range (hardware watchpoints or in-process instrumentation) |
| `capture_content` | Keep the bytes of chosen reads and writes (files, pipes, sockets), by path or descriptor |
| `logs` | The target's own output and log files, and the system's log lines about it |

### 4.2 Events

Every source is turned into one event shape: time, session, machine, pid, tid, process name, class, operation,
subject (path, key, address or endpoint), arguments, result, duration, and a call stack where the source has one.
Events go into one SQLite database per session, next to the session's captured content, dumps, pictures and video.

| Class | What is recorded |
|---|---|
| **process** | Start, exit and exit code of the target and every child; command lines, environment, parent, user, privilege |
| **thread, module** | Threads created and ended; libraries and drivers loaded |
| **syscall** | Each system call with decoded arguments and result, filterable by call, group (file, network, process, memory, ipc), path or descriptor; off, summarised (counts, errors, time per call) or full per session |
| **api** | Calls into the platform's libraries above the kernel (Windows API and COM, libc, Objective-C and Swift runtime, Java and ART on Android) with decoded arguments, buffers, return values and error codes, for the functions the agent chooses; needs instrumentation inside the target (§4.3) |
| **file** | Open, create, read, write, rename, delete, attribute and permission changes; sizes and offsets; contents on request |
| **registry / settings** | Windows registry opens, reads, writes and deletes; on macOS preference domains; on Linux dconf/GSettings and config files; on Android settings and shared preferences |
| **memory** | Allocations, mappings, protection changes, growth over time, leaks suspected; reads and writes of watched ranges; dumps |
| **network** | DNS lookups; TCP and UDP connections in and out with byte counts; listening sockets; packets (pcap) and decrypted HTTP(S) on request |
| **ipc** | Pipes, local sockets, D-Bus, COM/RPC, XPC, binder, where the platform can show them |
| **ui** | Windows and dialogs opened and closed, with their text; the foreground app; notifications |
| **log** | stdout and stderr, the app's log files, and the system log (journald, Event Log and debug output, unified log, logcat) |
| **problem** | Crash with a symbolised backtrace; hang; error dialog; early exit; a broken guard (below) |

What "memory reads and writes" means: recording every memory access of a running program slows it far too much to
be a default on any platform. Master Control records memory *activity* (allocations, mappings, protections, growth)
all the time, and exact reads and writes for the ranges the agent asks to watch, plus dumps on demand. Whole-program
access tracing is offered only where instrumentation makes it workable (**to prove**).

### 4.3 Where the events come from

| | Main source | Also |
|---|---|---|
| **Linux** | eBPF programs loaded by the root helper (syscalls, files, processes, sockets, memory calls), with stacks, built to run across kernels (CO-RE). Not built yet: today's source is the one in the next column | ptrace-based tracing of the user's own processes, with no root: every system call of the target's tree is seen and decoded, at the cost of slowing the target; `/proc` for snapshots; systemd-coredump for crashes; journald |
| **Windows** | ETW. Built: one real-time session carrying the kernel's process, file, registry and network providers and, in system-logger mode, its older process and memory events (which have the command line and the allocations the newer ones lack), read in the tracer itself; a file event is held until the event that ends its operation arrives, and filed with the result and the bytes really moved; other programs' events are dropped by process id before they are decoded. Registry keys are named by following each key back through the parent it was opened under. Windows doesn't say whether a file was opened to read or to write, only whether it was made, replaced or opened; a started program has a command line, not a list of arguments. To come: threads, images, stack walks, DNS, audit-API; system calls by name are not available from these events | In-process API hooking for call arguments ETW doesn't give (**to prove** what the syscall flag alone gives); Windows Error Reporting dumps; Event Log; debug output |
| **macOS** | Built, and needing no administrator rights: the target runs under a sandbox profile that forbids nothing and asks the system to report what it allows: programs started, files opened to read, made, written and removed, network connections and listening ports, services looked up. It reports these for the target and everything it starts, system programs included. The reports come through the system log (`log stream`) with the pid that did each thing; exits and how each process ended come from the kernel's process events (kqueue), arguments from the process table. This source doesn't give byte counts, failed calls, the address behind a connection (only its port), a rename as one event (the old name goes, the new one comes) or system calls (`syscalls: full` is refused). A target that sets up a sandbox of its own (a browser, an App Sandbox app, `sandbox-exec`) is refused it by the system inside this one, so it can't be watched this way. Measured on macOS 27.2: 2,000 file creations in a row all arrived; with several programs writing thousands of files at once the live log lost between a tenth and a sixth of the reports, and says nothing when it does | Built, used when this user may become root unasked (sudo without a password): processes and files come from Endpoint Security events, read from `eslogger`: every start with its parent and arguments, every exit with how it ended, files read, made, written (told when a written file is closed), renamed (as one event), removed, truncated, linked and re-moded, Unix-socket connections. The events are numbered, so the session's state says how many were lost (`events_lost`) and which source it used (`source`). The sandbox's reports then only add network ports and services; with `self_sandboxed` the sandbox is left out, the target runs as it is, and no network is recorded. `eslogger`'s output is not a promised format and may change with macOS. To come: addresses and bytes of connections, `fs_usage`-class tracing, the unified log, `sample`/`vmmap`/`leaks`, crash reports |
| **Android** | adb: logcat, `dumpsys`, Perfetto traces (scheduling, syscalls, memory counters, logs), `run-as` for debuggable builds | Heap profiling and in-process instrumentation for builds marked debuggable or profileable, which is what an app under development is; full syscall and file tracing on an emulator or rooted device. For a release build on an ordinary phone, less is visible (**to prove** how much) |
| **VM, agentless** | whatever the hypervisor exposes; introspection where it exists | none promised |

Instrumentation inside the target, for the **api** class, watched memory and call arguments the kernel sources
don't give, gets in one of three ways: injected into the running process by the helper (needs the same rights a
debugger needs), loaded by the app itself as a library in a test build, or on Android attached to a build marked
debuggable. It is off unless a session asks for it, because it changes the process it watches.

Network, on every platform: connections and DNS come from the tracing source above; packets from a capture limited
to the target's processes; decrypted HTTPS from a local intercepting proxy scoped to the target (per-process capture
on desktop, a local VPN on Android), with the test certificate installed only for the test.

### 4.4 Guards, baselines and the verdict

A session can carry rules, so a test fails by itself when the app misbehaves:

- **Allowed folders**: a write anywhere else is a problem.
- **Guards**: files, folders, registry keys or settings that must not change (the real app's data and settings).
- **Baseline**: learned from good runs; a new host contacted, program started or file touched outside it is a problem.
- **Limits**: memory growth, handle or descriptor count, CPU, run time.

`observe_stop` returns `ok` or `problem` with the list of problems, each pointing at its events. Paths and keys
match by whole components. A problem is reported once per kind and path, however often it happened. Opening a
file for writing counts as a change to it; reading a guarded file doesn't. A crash names the signal and where the
program was (`program+0x1a2b`) on Linux, and the exit code and what it means (`0xC0000005`, access violation) on
Windows; ending with an error code of the program's own, or being stopped by `observe_stop`, is not a crash. On
Windows a guard can be a registry key (`HKCU\Software\App`); names there match whatever the capitals.

### 4.5 Cost and safety of tracing

- Each event class can be switched on or off per session; full syscall tracing is opt-in. `status` reports dropped
  events, so a gap is never silent.
- Only the target's process tree is recorded, filtered where the events are collected, not afterwards: a whole
  Windows desktop produces tens of thousands of events a second, nearly all from other programs.
- Another program touching the target's files or keys (an antivirus scan, an indexer) is recorded as an event on
  that file or key, marked as coming from outside the target.
- Only the target's process tree is traced in full. Captured content, packets and decrypted traffic stay on the machine
  under test until the agent pulls them, and are deleted with the session.

## 5. Interfaces and security

- **MCP**: stdio from the controller; streamable HTTP from the agent. Pictures come back as images; long results
  are paged.
- **HTTP API**: JSON under `/v1/`: `POST /v1/<tool>` with the arguments as a JSON object, server-sent events for
  streams. Whole files go as raw bytes: `PUT` and `GET /v1/files?path=…`. Errors come back as
  `{"error": {"code", "message"}}` with a matching HTTP status.
- **CLI**: `mctl <tool> [machine] …`, plus `mctl install`, `mctl machines`, `mctl test`.
- **Master and worker.** The master shows an eight-digit pairing code while its Pair screen is open (`mctl pair`,
  ten minutes unless told otherwise), listens for workers, and announces where it is. A worker that is given the
  code (`mc-agent pair CODE`, with the master's address if it isn't found on the network) runs a
  password-authenticated key exchange with the master on the code, so a listener learns nothing and a wrong code
  gets nowhere; each side then proves to the other that it had the code before either keeps the key. One code
  admits any number of workers while the window is open; five wrong tries and the master shows a new one. From then
  on every message between them is sealed with their shared key, numbered within a conversation so a recorded
  message can't be sent again, and each answer is tied to its request. The worker answers no other master until
  it is told to forget this one, by `mc-agent unpair` or by its master cleaning up. The worker's network port
  (47901) takes only sealed messages: the plain API and the token are not served there. `mctl scan` lists the
  workers heard on the network (announcements go to port 47902) and updates the address of a paired worker that
  moved. What the master asks is in the worker's audit log, marked as coming from the master.
- **The app.** `master-control` is one program per platform: started plainly it opens a window that asks whether the
  machine is a Master or a Worker and then offers that role's Start, Pair and Cleanup, with a line saying what is
  installed and paired, and the lines of whatever is running. The master's Pair shows the code and the workers as
  they pair; the worker's asks for the eight digits. Cleanup asks first, with Cancel as the safe choice. The
  window is drawn by the program itself (its own shapes, the app's icon, and a built-in font, Noto Sans), so it
  is the same on every system and needs no toolkit. It is drawn at the display's own zoom, so it is sharp on
  dense screens, and carries the icon in the title bar and the task switcher. On macOS it is one AppKit window in
  `Master Control.app` (`macos/scripts/build.sh` builds and signs it), with only the app's own menu in the menu bar: plain X11 on Linux, one Win32 window on Windows. The same program is the
  worker and the master's command line when it is installed under those names (`mc-agent`, `mctl`), or started
  with `agent` or `master` as its first word. On Windows it asks for admin rights when it opens.
- **Who a machine is.** Every machine running Master Control, master or worker, is identified by three things
  together: its host name, its operating system's release, and the hardware (MAC) address of its main network
  interface, written as one id (`example-host|Ubuntu 24.04.3 LTS|aa:bb:…`). `status` gives it, workers announce
  it, the master records it for each worker, and the app shows it at the top of its window. The id changes when a
  machine is renamed or its system updated; the master then follows the worker by its hardware address and takes
  the new id the next time they talk. On macOS the address is the interface's real one, not the private address
  it may show a network.
- **Status, live.** The first screen has a Status section under the two roles, looked at again every second:
  what this machine is the master and the agents it is set up for, or a worker and whose (a master is never
  described as "not a worker": it gives the agents on it everything a worker would); each worker and whether
  it answers (a quick connection to its port); and the master's latest calls. A line's light flickers, unevenly like a disk's, while there is traffic for
  it: a call to that worker under way or just ended, any call for the master's own machine, and on a worker
  the requests coming in from its master. The calls shown are which machine, which tool, how it ended and
  how long it took, never what was sent, read from the activity file the master keeps (`activity.log` in its
  folder, cut to its newer half when it passes half a megabyte).
- **The AI agents.** The master's Start gives every agent found on the machine its access: Claude Code, the
  Claude app, Codex, Antigravity, Gemini CLI, Cursor, Windsurf and VS Code, each found by its settings folder,
  its app or its command (looked for in the folders installers use as well as on the search path, since an app
  opened from the desktop has a short one), and each given Master Control in its own list of MCP servers, in
  that list's own shape. Nothing else in those files is changed, and each is kept as it was beside it the
  first time. `mctl agents` lists what is found and whether it is set up; for any other agent it prints the
  command to add by hand. Cleanup takes every entry out again. A guide written for the agents themselves
  (`AGENT-GUIDE.md`: how to connect, the rules that matter, what each refusal means, recipes) travels inside
  the program: `mctl guide` prints it, the MCP tool `guide` returns it, and Start leaves a copy beside the
  installed program. A test keeps it naming every tool the server offers.
- **Start, Pair, Cleanup.** Each role has the same three actions. Start installs what is missing and is safe to
  repeat: on a worker (`mc-agent start`) the program, its start with the desktop session and its admin rights; on
  the master (`mctl start`) the program and the AI agents' access to it (today Claude Code's MCP registration).
  Cleanup removes all of it, pairings and recorded data included: a worker's (`mc-agent cleanup`) removes its
  start-up entry, firewall rule and folder; the master's (`mctl cleanup`) first tells every worker to forget it,
  then removes the agents' access, its folder and its program.
- The agent's own API listens only on loopback (port 47900 unless told otherwise). Every request carries a token
  (`Authorization: Bearer …`) kept in a file only that user can read; `mc-agent token` prints it. Requests that
  come from a web page, or name any host but this machine, are refused.
- The agent's folder holds the token, the audit log, jobs' output and `agent.json` (the port in use):
  `~/Library/Application Support/Master Control` on macOS, `~/.local/share/master-control` on Linux,
  `%LOCALAPPDATA%\Master Control` on Windows, or wherever `MC_HOME` or `--home` says.
- Everything the agent does is written to its own audit log: how the request came, the tool, its arguments, when,
  the result. File contents, a command's input and environment values are left out of it.
- `exec` answers with the end of a command's output (256 KB of each stream); `job` reads all of it in pieces. A
  command that runs past its time limit is stopped together with everything it started.
- The elevated helper accepts requests only from the agent, and only the fixed set of tracing and capture jobs.

## 6. Install and platforms

The agent, helper and controller are written in Rust. Installing gives Master Control admin rights once, so nothing it does later needs a password or an elevation
prompt. `mc-agent start` copies the program into place and starts it as a worker with the desktop session:
on Linux a systemd user service, with admin rights through sudo (the install says what to add if sudo asks this
user for a password); on macOS a launch agent in the desktop session that launchd keeps running, with admin rights
through sudo in the same way (a Mac worker does commands, files, screen, input, the accessibility tree and observation); on Windows a scheduled task at sign-in with highest privileges and no window, plus a
firewall rule that lets the master reach the worker's port on private networks only, or with `--any-network` on
public ones too (for a machine whose network Windows has marked Public). `mc-agent cleanup` undoes
it and removes the worker's folder. `exec` takes `elevated` to run a command with admin rights. On macOS a worker that listens on the network makes the system's own firewall ask once whether to let it; that
answer is the user's. One self-contained program per platform, nothing else to install: a portable `.exe` on Windows, a signed app bundle
on macOS, a single binary with a user service on Linux (the agent needs the real session and the kernel, so it is not
a Flatpak). Android needs only adb on the controlling machine. `mctl install <machine>` puts the agent on a machine
over SSH and starts it.

| Platform | Minimum, to be confirmed while building |
|---|---|
| Linux | A kernel with BTF and eBPF CO-RE; X11 and Wayland sessions |
| Windows | Windows 10 and 11, x64 and ARM64 |
| macOS | macOS 15 and later, Apple silicon |
| Android | Android 10 and later; emulator, rooted and unrooted devices, with what each can show |

## 7. Reference apps

Studied on 2026-11-11, open-source apps from their source and closed-source ones from their vendors' documentation;
nothing of theirs is copied. Master Control follows the behaviour noted here
with its own code.

| App | What it shows us |
|---|---|
| Cua Driver (trycua/cua, MIT) | One program serving MCP and a CLI; each platform's own capture, input and accessibility stack; every action answered as delivered, refused with a reason, or unsupported |
| Windows-MCP (MIT) | A Windows tool set over stdio and HTTP: UI Automation tree, input, shell, registry, processes |
| scrcpy (Apache-2.0) | Android control with nothing installed: a server pushed over adb and started through the shell, a tunnel, injected input, an encoded screen stream |
| mobile-mcp (Apache-2.0) | The tool list a phone needs; plain adb as the simple route |
| PiKVM (GPL-3.0) | A physical machine with no software on it: HDMI capture, USB HID gadget, virtual media, power control |
| Anthropic computer-use demo (MIT) | The action set models are trained to use for screens |
| Fibratus (Windows) | What ETW gives: process, thread, image, file, registry, network, memory, DNS and API events with call stacks; filters and rules over them |
| Process Monitor for Linux (MIT) | eBPF syscall tracing filtered by process and call, stored in SQLite and reopened later |
| Tracee (Apache-2.0) | eBPF event families (syscall, network, security) and capture of file contents and packets |
| CAPE Sandbox | An agent inside the guest plus API hooking, ETW, TLS keys, screenshots; one behaviour report per run |
| DRAKVUF | The same kinds of events from the hypervisor, with no agent in the guest |
| mitmproxy (MIT) | Capturing one process's traffic on the same machine, decrypted |
| PCAPdroid (GPL-3.0) | Per-app capture on Android without root through a local VPN |
| Objective-See ProcessMonitor (GPL-3.0) | macOS Endpoint Security as the event source |
| Frida | The three ways into a process (injection, a library the app loads, a debuggable Android build) and what is possible inside: function hooks, following a thread instruction by instruction, a per-page memory access monitor |
| strace (LGPL) | Tracing without root; filters by call group, path and descriptor; dumping the data read and written; a summary of counts, errors and time; making chosen calls fail |
| Perfetto (Apache-2.0) | Android and Linux tracing started over adb from a config, with named data sources and SQL over the result; what a release build allows |
| Sysmon for Linux (MIT) and Sysmon | A fixed, documented list of event types with include and exclude rules per type; eBPF that adapts to the kernel it finds |
| Process Monitor (documentation, and run on Windows 11) | The everyday view to match: file, registry and process activity live, a stack per operation, filters that hide without discarding, a log that can be reopened, logging from boot. Each event as time, process, operation, path, result and a readable detail line; run and exported from the command line |
| API Monitor (documentation and its definition files; not run) | API and COM calls decoded from definition files: parameters, structures, buffers sized from other arguments, error codes, call tree. A definition per library that names each call's parameters, types, flags, success value and error decoding |
| Instruments (documentation only) | Recording by template from the command line, attached to one process or all, exported for other tools |

The `master-control` skill Herbert's projects use today is the prototype: a token-protected agent on Linux and
Windows, SSH-tunnelled CLI and MCP tools, X11 and Win32 input and pictures, and an observer built on bpftrace, ETW,
core dumps and Windows Error Reporting. Master Control replaces its scripts and keeps its tool names working.
