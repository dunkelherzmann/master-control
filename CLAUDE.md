# Working in Master Control

Herbert's rules are in the `herberts-preferences` skill; this file holds what is particular to this project.
The spec is `specs.md` (his name for it here); `bugs.md`, `enhancements.md` and `pending.md` are at the root;
the reference study is in `.investigation/` (untracked). The repo is public on GitHub
(`dunkelherzmann/master-control`, since 2026-11-12): nothing private goes into tracked files (no machine names,
LAN addresses, local paths or pictures of real desktops; the machines' ssh names live in the untracked
`scripts/hosts.env`). The public history starts at 0.2; what came before is in the local branch
`local-history-do-not-push`, which holds private pictures and names: push `main` by name, never `--all` or
`--mirror`.

## What it is

An installable agent serving an HTTP API and MCP so coding agents can see and drive a machine (screen,
keyboard, mouse) and observe everything a target app does (syscalls, files, registry, memory, network, logs),
on Android, Linux, Windows and macOS. Rust core; it productises the `master-control` skill.

- **`AGENT-GUIDE.md`** is for agents that use Master Control (this file is for working on it). It is built into
  `mctl`; a test fails when it doesn't name a tool, so a new tool gets a line there in the same change.
- **Layout:** Cargo workspace: `core/` (event store), `agent/` (the `mc-agent` program), `app/` (the
  `master-control` window, self-drawn, icon `design/icon.svg`).
- **A master is not "not a worker"** (his correction, 2026-11-11): the master gives agents full use of its own
  machine as well as of its workers, so the app never says the master machine lacks the worker role.
- **Master and workers** (his design): the master (`mctl`, the Mac) has full control of itself and its workers;
  a worker (`mc-agent start`, then `mc-agent pair CODE` with the code `mctl pair` shows) is controlled by its
  master only; `cleanup` on either removes everything. The app is one window over these: Master or Worker, then
  Start / Pair / Cleanup.
- **Observation** is a helper process (`mc-agent observe-run`): ptrace on Linux, ETW on Windows (needs admin);
  its test target is `agent/src/bin/mc-test-target.rs`.

## Build, test, install

- `cargo test --workspace` on the Mac; `scripts/check.sh` on all three machines (sends the last commit to Linux
  and Windows over ssh; `-w` sends the working tree). It runs clippy on each machine: code gated per system
  leaves helpers and imports unused elsewhere, which only that system's lint shows.
- Linux GUI tests use Xvfb. Windows GUI tests must run in the desktop session (`mctl exec --cwd`): ssh alone has
  no screen.
- `macos/scripts/build.sh` makes `dist/Master Control.app`; the Linux file is the static musl build.
- `scripts/place.sh [mac linux windows]` builds the last commit and installs it on all three (master, apps,
  workers), and fails unless every copy then reports the new version. Use it instead of doing those steps by hand.
- **Installed copies:** `/Applications` on the Mac, the desktop on Linux and Windows. Workers: Linux (service
  `master-control`) and Windows (task "Master Control"), both paired; the Windows firewall rule covers public
  networks too (his choice).
- **The master** is `~/.local/opt/master-control/mctl` (`machines`, `call MACHINE TOOL JSON`, `mcp`) and is Claude
  Code's `master-control` MCP server on the Mac. After rebuilding, copy the new `target/release/mctl` there. The
  old skill's `mctl` stays on PATH for `mctl test` and the projects' test scripts.
- A remote install that builds `origin/main` or a sent commit: check on that machine that it built the commit
  you meant before calling it installed.

## Traps

- **Mac tests must not really act.** Tools run from a Claude session inherit Claude's Screen Recording and
  Accessibility permissions, so an input tool would move his mouse: Mac tests call input or screenshot tools
  only when the permission is missing. `MC_NO_ACTIVATE` did not stop the app's window becoming active.
- **Trying a launchd agent on the Mac** without raising the firewall prompt: set its listen address to loopback
  with `launchctl setenv` before bootstrap and `unsetenv` after; check `socketfilterfw --getglobalstate` first.
- **Merging and building in one command line:** join the steps with `&&`, never `;`. On 2026-11-11 another
  session had committed to `main`, the fast-forward merge refused, and the build and install that followed ran
  from the wrong commit. After installing anywhere, prove the new build by something only it has (a new tool in
  `status`, the version), and build each program by name: `cargo build -p a --bin a -p b` builds only `a`'s
  program.
- **A report or state read straight after an accessibility action can be the old one** (UI Automation's Toggle,
  GTK through AT-SPI): wait for the change, briefly, before answering.
- **Input to a Mac app in the background** (public calls only): key events posted to the app's pid arrive in its
  focused field; mouse events posted the same way are ignored by AppKit; a Command shortcut arrives only some of
  the time unless the window is first made the app's main window (`AXMain`) and the shortcut's menu item is
  pressed. A shortcut needs the app to have that menu item: the test window has an Edit menu for this reason.
  The gated Mac test (`MC_MAC_GUI_TESTS=1`) acts only on its own window, which stays behind.
- **Linux, a tracer started through sudo** (following a running program): it can't be signalled by the agent, so
  it is stopped by a `stop` file in the session's folder; `setfsuid`/`setfsgid` to the invoking user keeps what
  it writes the user's while the right to trace stays. Install the stop-signal handler before any early return
  into another session loop.
- **Windows runs only a few kernel trace sessions at once** (error 1450): more observation tests in parallel
  than that fail unless the recorder waits for a place.
- **Tests that run the master's Start or Cleanup must set `MC_AGENTS_HOME`** to a scratch folder: without it
  they edit the real settings of every AI agent on the machine (`~/.claude.json`, `~/.codex/config.toml`, …). The same
  goes for trying `mctl start` by hand: on 2026-11-11 a hand run with only `MC_INSTALL_DIR` set pointed the real
  agents at a scratch copy until Start was run again from the installed app.
- **macOS windows:** the window list and the accessibility layer name the same window differently (a browser
  tab playing sound has a mark in one); match them by `_AXUIElementGetWindow` (unpublished but present), then
  title, then place. `AXWindows` lists only the windows on the desktop in view.
- **Two clicks on one spot within the double-click time are a double click,** even from two tool calls: don't
  click an element to give it the keyboard when it already has it.
- **macOS crashes are caught by the recorder** (`mac_crash.rs`): it sets itself as the handler for fatal crashes
  (`EXC_CRASH`), children inherit that, and the kernel's message brings the thread and task to read the stack
  from. The reply must be "not handled" or the process never dies. A Mac program's file has no base address of
  its own (`relative_address_base` is 0): its first loaded part's address is the base.
- **macOS crash reports** (`~/Library/Logs/DiagnosticReports/NAME-date.ips`) arrive about two seconds after a
  crash, but a program that crashes repeatedly gets one for its first crash and none for a good while after.
  It is not a limit on files kept: clearing the reports and the reporter's note
  (`~/Library/Application Support/CrashReporter/NAME_*.plist`) changed nothing (an earlier note here said
  otherwise; it was a guess). A copy of the program under a new name is reported at once, which is how the
  crash test gets a report every run. The test removes exactly the reports and notes it caused (his go-ahead,
  2026-11-11).
- **`mctl call` is one process per call,** so the master's own tools keep no last picture between calls: give
  desktop positions, not window-relative ones.
- **Test GUI input against a real toolkit app as well as a raw fixture window:** a raw X11 window accepted a
  borrowed-keycode character that GTK 4 dropped. Run a new suite several times before merging; flaky tests
  showed only on reruns.
- **X11:** a window must select ButtonPress to receive ButtonRelease (the release goes to whoever took the
  press).
- **Fonts:** embed only a static file per weight (`fc-scan` says `variable=False`); Ubuntu's R/M/B files are one
  variable font, which the simple rasteriser draws at regular weight only.
- **Tables of platform facts** (syscall numbers) are generated from the target machine's own headers with a
  committed script (`scripts/gen-syscalls.sh`), never typed from memory.
- **ETW:** read provider event layouts from the machine (`Get-WinEvent -ListProvider`) before decoding; a
  system-logger-mode session can carry manifest providers and classic kernel flags together.
- **Test arguments that pack a path with other data** must not use a colon as separator: `C:\` splits there and
  the program writes somewhere else.
- **Porting a tracer feature:** list the reference's behaviours from our own source, read the target platform's
  event layouts live, and read an open-source tool's source for the mechanism.
