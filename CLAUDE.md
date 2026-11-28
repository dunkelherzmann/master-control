# Working in Master Control

Herbert's rules are in the `herberts-preferences` skill; this file holds what is particular to this project.
The spec is `specs.md`; `bugs.md`, `enhancements.md` and `pending.md` are at the root;
the reference study is in `.investigation/` (untracked). The repo is public on GitHub
(`dunkelherzmann/master-control`, since 2026-11-12): nothing private goes into tracked files (no machine names,
LAN addresses, local paths or pictures of real desktops; other names for the machines go in the
untracked `scripts/hosts.env`). The public history starts at 0.2; what came before is in the local branch
`local-history-do-not-push`, which holds private pictures and names: push `main` by name, never `--all` or
`--mirror`.

## What it is

An installable agent serving an HTTP API and MCP so coding agents can see and drive a machine (screen,
keyboard, mouse) and observe everything a target app does (syscalls, files, registry, memory, network, logs),
on Linux, Windows and macOS. Rust core; it productises the `master-control` skill.

- **`AGENT-GUIDE.md`** is for agents that use Master Control (this file is for working on it). It is built into
  `mctl`; a test fails when it doesn't name a tool, so a new tool gets a line there in the same change, and in the
  two tool lists `agent/tests/api.rs` pins.
- **Layout:** Cargo workspace: `core/` (event store), `agent/` (the `mc-agent` program), `mctl/` (the master),
  `app/` (the `master-control` window, self-drawn until its egui rewrite in `enhancements.md`; icon `design/icon.svg`).
- **Master and workers** (his design): the master (`mctl`, the Mac) has full control of itself and its workers,
  so the app never says the master lacks the worker role (his correction, 2026-11-11);
  a worker (`mc-agent start`, then `mc-agent pair CODE` with the code `mctl pair` shows) is controlled by its
  master only; `cleanup` on either removes everything. The app is one window over these: Master or Worker, then
  Start / Pair / Cleanup.
- **Observation** is a helper process (`mc-agent observe-run`): ptrace on Linux, ETW on Windows (needs admin);
  its test target is `agent/src/bin/mc-test-target.rs`.

## Build, test, install

- `cargo test --workspace` on the Mac; `scripts/check.sh` on all three machines (sends the last commit to Linux
  and Windows through Master Control with `scripts/mc-exec`; `-w` sends the working tree). It runs clippy on each machine: code gated per system
  leaves helpers and imports unused elsewhere, which only that system's lint shows.
- `scripts/mutants.sh` (cargo-mutants on `mc-core`; `-f FILE`, `--all`) before merging a core change; it reports.
- Linux GUI tests use Xvfb (they set `DISPLAY` and clear `WAYLAND_DISPLAY`, which picks the X11 backend).
  Windows GUI tests need the desktop session, which is where Master Control's `exec` runs them.
- `macos/scripts/build.sh` makes `dist/Master Control.app`; the Linux file is the static musl build, and the Linux
  worker is that same file (`master-control agent start`); there is no separate Linux `mc-agent` to ship.
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

- **Another session works here too** and merges its own pull requests: before tidying trackers or branches, fetch
  and read `gh pr list --state all`, then change only what is still out of date.

- **Mac tests must not really act.** Tools run from a Claude session inherit Claude's Screen Recording and
  Accessibility permissions, so an input tool would move his mouse: Mac tests call input or screenshot tools
  only when the permission is missing. `MC_NO_ACTIVATE` did not stop the app's window becoming active.
- **Trying a launchd agent on the Mac** without raising the firewall prompt: set its listen address to loopback
  with `launchctl setenv` before bootstrap and `unsetenv` after; check `socketfilterfw --getglobalstate` first.
- **Sources sent to another machine** are unpacked with `tar -m`: with the sender's file dates, cargo there takes a
  file older than its last build as unchanged and tests the old code (seen 2026-11-28; `check.sh` does it).
- **Merging and building in one command line:** join the steps with `&&`, never `;` (a refused merge once left
  the build on the wrong commit). Prove an install by something only the new build has (the version), and build
  each program by name: `cargo build -p a --bin a -p b` builds only `a`'s program.
- **A report or state read straight after an accessibility action can be the old one** (UI Automation's Toggle,
  GTK through AT-SPI): wait for the change, briefly, before answering.
- **Input to a Mac app in the background** (public calls only): key events posted to the app's pid arrive in its
  focused field; mouse events posted the same way are ignored by AppKit; a Command shortcut arrives only some of
  the time unless the window is first made the app's main window (`AXMain`) and the shortcut's menu item is
  pressed. A shortcut needs the app to have that menu item: the test window has an Edit menu for this reason.
  The gated Mac test (`MC_MAC_GUI_TESTS=1`) acts only on its own window, which stays behind.
- **Linux, a tracer started through sudo** can't be signalled by the agent: a `stop` file in the session's folder
  stops it; `setfsuid`/`setfsgid` to the invoking user keeps what it writes the user's while the right to trace
  stays. Install the stop-signal handler before any early return into another session loop.
- **Windows runs only a few kernel trace sessions at once** (error 1450): more observation tests in parallel
  than that fail unless the recorder waits for a place.
- **A test agent that sends input must have asking off** (`{"active_within_s":0}` in its home's `take-over.json`,
  as `agent/tests/common` and `api.rs` do): otherwise someone using the machine during the run gets the take-over
  question from the test, and a Cancel fails it (2026-11-27, on Windows).
- **Tests that run the master's Start or Cleanup must set `MC_AGENTS_HOME`** to a scratch folder: without it
  they edit the real settings of every AI agent on the machine (`~/.claude.json`, `~/.codex/config.toml`, …). So does
  `mctl start` by hand: one with only `MC_INSTALL_DIR` set pointed the real agents at a scratch copy (2026-11-11).
- **macOS windows:** the window list and the accessibility layer name the same window differently (a browser
  tab playing sound has a mark in one); match them by `_AXUIElementGetWindow` (unpublished but present), then
  title, then place. `AXWindows` lists only the windows on the desktop in view. A system dialog (layer 8, owner
  `UserNotificationCenter`) gives no window number and no title, and the window list counts its shadow (75 points
  a side on macOS 27) where accessibility doesn't: it is matched by sitting in the middle of the listed frame.
- **Two clicks on one spot within the double-click time are a double click,** even from two tool calls: don't
  click an element to give it the keyboard when it already has it.
- **macOS crashes are caught by the recorder** (`mac_crash.rs`): it sets itself as the handler for fatal crashes
  (`EXC_CRASH`), children inherit that, and the kernel's message brings the thread and task to read the stack
  from. The reply must be "not handled" or the process never dies. A Mac program's file has no base address of
  its own (`relative_address_base` is 0): its first loaded part's address is the base.
- **macOS crash reports** (`~/Library/Logs/DiagnosticReports/NAME-date.ips`) arrive about two seconds after a
  crash, but a program that crashes repeatedly gets one only for its first crash for a good while, whatever is
  cleared. A copy under a new name is reported at once, so the crash test makes one each run and removes exactly
  the reports and notes it caused (his go-ahead, 2026-11-11).
- **`mctl call` is one process per call,** so the master's own tools keep no last picture between calls: give
  desktop positions, not window-relative ones.
- **Test GUI input against a real toolkit app as well as a raw fixture window:** a raw X11 window accepted a
  borrowed-keycode character that GTK 4 dropped. Run a new suite several times before merging; flaky tests
  showed only on reruns.
- **GNOME on Wayland** (the Linux machine runs Ubuntu 26.10 with GNOME Shell 51, Wayland only): screen, input, windows and clipboard
  go through our shell extension (`linux/gnome-extension/`, embedded in `mc-agent`, installed by `start`). Its
  `API` number in `extension.js` and `gnome.rs` goes up together with any change to its methods. GNOME Shell loads
  a new or changed extension only at sign-in, and GJS caches modules by path, so reloading it in place keeps the
  old code. To run a change without signing out, run `linux/scripts/load-gnome-extension.sh` on the Linux machine
  (after `mc-agent start` put the new files in place): it loads a copy from a new folder named after the UUID
  through Looking Glass, typed with Mutter's RemoteDesktop D-Bus API, which needs no prompt.
  Raising `API` cuts the installed Linux worker off the desktop (it refuses an older extension): after `place.sh`,
  run the loader there at once (send it with `scripts/mc-exec linux --put`).
  Each new GNOME release must be added to `shell-version` in `metadata.json`, or the shell won't load it; GNOME 51
  also dropped `Clutter.get_default_backend()` (the backend now comes from `global.stage.context`).
  `org.gnome.Shell.Screenshot` and `Introspect` refuse callers that aren't GNOME's own. The session locks after 5
  idle minutes; `sudo loginctl unlock-session N` unlocks it without the password.
- **AT-SPI and Flatpak:** a sandboxed app's bus connection is its `xdg-dbus-proxy`, so match it by the instance id in
  `/proc/PID/root/.flatpak-info`, not by pid. `flatpak kill INSTANCE` takes a moment; killing the shell that ran
  `flatpak run` leaves the sandbox running. AccessKit apps answer `GetRole` but not `GetRoleName`: the names come
  from `atspi_roles.rs`, generated by `scripts/gen-atspi-roles.sh`. The gated test
  `a_flatpak_windows_controls_are_read_with_their_roles` needs `MC_FLATPAK_TREE_APP` (an installed app id and its
  arguments) as well as `MC_GNOME_GUI_TESTS=1`.
- **AT-SPI positions on Wayland** are relative to the window. Count from the client rectangle
  (`frame_rect_to_client_rect`) when the shell draws the title bar (`decorated`, Xwayland apps), else from the frame:
  for an app that draws its own frame the client rectangle includes its invisible shadow border. GTK 4 (4.24 and its
  main branch) adds up each element's bounds inside its parent widget, counted from the parent's content box, so
  every container's border and padding is lost (24 pixels across in a libadwaita dialog) and no AT-SPI call gives
  the true place; it also refuses `GrabFocus`. A plain click by id on a GTK 4 element uses its `click` action, or
  Tab for a field (`atspi::click_without_pointer`); never its `activate`, which is Enter in a field.
  Chromium's accessible title leaves out the profile name and the zero-width space the shell's title has.
- **The GNOME test** drives the signed-in session, so it runs only when asked: on the Linux machine,
  `MC_GNOME_GUI_TESTS=1 cargo test -p mc-agent --test desktop_gnome` (the extension must be running, the screen
  unlocked).
- **What `exec` starts on Linux runs in a scope of its own** (`systemd-run --user --scope`, with
  `--expand-environment=no` or systemd rewrites every `$`), because systemd stops everything in a service's cgroup,
  `setsid` included, whenever the worker service stops or restarts (each install, each log-out).
- **Test polkit with `pkcheck --action-id org.freedesktop.policykit.exec --process $$`** (0 allowed, 2 asks), never
  `pkexec`: it opens a password dialog on the person's desktop.
- **X11:** a window must select ButtonPress to receive ButtonRelease (the release goes to whoever took the
  press).
- **Fonts:** embed only a static file per weight (`fc-scan` says `variable=False`); Ubuntu's R/M/B files are one
  variable font, which the simple rasteriser draws at regular weight only.
- **Tables of platform facts** (syscall numbers) come from the machine's own headers (`scripts/gen-syscalls.sh`).
- **ETW:** read provider event layouts from the machine (`Get-WinEvent -ListProvider`) before decoding; a
  system-logger-mode session can carry manifest providers and classic kernel flags together.
- **Test arguments that pack a path with other data** never use a colon as separator (`C:\` splits there).
- **Porting a tracer feature:** list the reference's behaviours from our source, read the platform's event layouts
  live, and an open-source tool's source for the mechanism.
