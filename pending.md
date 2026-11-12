# Pending

What is still to decide, prove and build, in order. Tick an item with its PR once it is built and tested.

## Decisions for Herbert

- [ ] **Linux packaging for the app.** A Flatpak's sandbox is at odds with tracing, input and services. Tried
  2026-11-11: the agent and the master build as fully static programs (musl; 6.5 MB and 8 MB, no shared libraries)
  and run. Proposed: one static program for Linux, its set-up window drawn by the program itself over X11.

- [x] **Language.** Rust for the agent, helper and controller (decided 2026-11-11), with a small Java server for
  Android pushed over adb where the platform requires it.
- [ ] **First platform.** Proposed: Linux and Windows first, since the prototype skill already runs there and those
  machines are free for testing; then macOS; then Android.
- [x] **GitHub repository.** Work stays local; nothing goes to GitHub until a functional, tested prototype is ready
  (decided 2026-11-11). Then: `dunkelherzmann/master-control`, private, with his yes.
- [ ] **Name.** "Master Control" is tentative.
- [ ] **Agentless back ends** (VM through the hypervisor, physical machine through a KVM-over-IP device): in the
  first version, or after the four agents work?

## To prove before promising (one probe each)

- [ ] macOS: which Endpoint Security events an unpaid-signed tool can receive; whether the stock `eslogger` as root
  covers the need; what System Integrity Protection blocks. Spec §4.3
- [ ] Android: what can be observed of a release build on an unrooted phone, against a debuggable build and a rooted
  emulator. Known from Perfetto's and Frida's source: debuggable or profileable builds allow heap profiling and
  in-process instrumentation without root; the release-build case is still open. Spec §4.3
- [ ] Memory: whether whole-program read and write tracing is usable anywhere, or only watched ranges. Spec §4.2
- [ ] Windows: what the ETW system-call flag gives on Windows 11, and whether API hooking is needed for arguments.
  Spec §4.3
- [ ] Linux: Wayland capture and input on Ubuntu GNOME without a prompt on every run. Spec §3.3
- [ ] VMs: which hypervisors first, and what each exposes for display and input. Spec §2
- [ ] MCP: the agent and the master speak protocol versions 2024-11-05 to 2025-06-18. Claude Code connects to the
  master (checked 2026-11-11); the newer protocol revision is still to read. Spec §5

## Build order (proposed)

1. [x] Project skeleton: workspace, `.gitattributes`, check script for the three machines, event schema and store
   (2026-11-11; `scripts/check.sh`)
2. [x] Agent core: HTTP API, token, MCP, audit log, `status`, `exec`, jobs, `push`/`pull` (2026-11-11; 34 tests
   pass on the Mac, Linux and Windows). Left for later steps: `exec` with admin rights (needs the helper), streams
   over server-sent events (comes with observation), running as a service at sign-in (comes with `mctl install`)
3. [x] Control on Linux (X11) and Windows: screenshot, windows, focus, pointer and keyboard with focus checks
   (2026-11-11; 48 tests pass on Linux and Windows, 40 on the Mac)
   - [x] Clipboard (`clipboard_get`, `clipboard_set`) on both (2026-11-11). Text only; saving and restoring the
     clipboard around a test is still to do
   - [ ] `move_window`, `close_window`, `apps`, `install`, `launch`, `stop`, `record_start`/`record_stop`
   - [ ] Linux: tests under a window manager (focus through the window manager is only smoke-tested in GNOME, by
     listing windows); several monitors; HiDPI
   - [ ] Windows: a window with higher rights in front (the refusal is written, not tested); several monitors with
     different scaling; pictures of windows GDI can't capture
   - [ ] `focus_refused` has no test on either platform: nothing in the test set-up refuses focus
4. [x] Accessibility tree, `find`, `wait_for` on Linux and Windows, and actions aimed at an element (2026-11-11;
   52 tests pass on Linux and Windows)
   - [ ] Linux is tested with one GTK 4 app (zenity); GTK 3, Qt, Electron and Chromium apps are not tried yet
   - [ ] Linux: GTK 4 reports places that are a few pixels off for some widgets; centres still land inside them
   - [ ] Windows is tested with classic Win32 controls; WPF, WinUI, WebView2 and Electron apps are not tried yet
   - [x] Element actions beyond the pointer: `set_value`, `toggle`, `expand`, `select` and `press` without moving
     the pointer, tested on Linux and Windows
   - [ ] Large trees: reading is one request per property; cache or batch before it is used on big apps
5. [x] Observation on Linux, first source (2026-11-11; 64 tests pass on Linux): a ptrace tracer for a started
   command, with process, file, network, memory and system-call events, the target's output, crashes, allowed
   folders, guards and the verdict. Chosen before eBPF because it needs no root and no kernel toolchain.
   - [ ] eBPF source (root helper), for speed and for attaching to anything; decide the toolchain (aya or libbpf)
   - [ ] Attach to a running process, by pid or app name
   - [ ] How much the tracer slows a target is not measured yet
   - [ ] ARM64 (the tracer reads x86-64 registers and call numbers)
   - [ ] `stream`, `snapshot`, `stack`, `dump`, `watch_memory`, `capture_content`
   - [ ] Baselines and limits (memory growth, descriptors, CPU, run time); hangs
   - [ ] DNS names for addresses; packets; decrypted HTTPS (step 8)
   - [ ] Settings (dconf, config files as their own class), ipc, ui and log events; the system log
   - [ ] Full call stacks for events and crashes (today a crash gives one place)
   - [ ] Deleting a session and its files; sessions from before an agent restart can't be read by the tools
6. [x] Observation on Windows (2026-11-11; 59 tests pass on Windows): an ETW tracer for a started command, with
   process, file, registry and network events, the target's output, crash exit codes, allowed folders, guards
   (files and registry keys) and the verdict. Needs admin rights.
   - [ ] A session ends about two seconds after its target does, waiting for the trace buffers; events arrive up
     to a second late
   - [ ] Events lost when the trace buffers overflow are not counted yet
   - [ ] A registry key opened before the session started (an inherited handle) is named only from that key down
   - [x] Brought level with Linux where Windows can tell (2026-11-11, from a study of the Linux tracer against
     Fibratus and the machine's own event layouts): command lines, memory allocations and frees, the result of
     each file operation and the bytes really read and written, made / replaced / opened for each open
   - [ ] Still behind Linux: opens don't say read or write (Windows doesn't report it); no memory protection
     changes or file mappings; no system calls (not available by name from ETW as read; to prove another way);
     no thread or image events; no call stacks
   - [ ] Where a crash happened (a dump from Windows Error Reporting); hangs; error dialogs
   - [ ] Attach to a running process; `stream`, `snapshot` and the rest, as on Linux
   - [ ] The Windows machine's desktop is shared with other projects' GUI tests: one run in four failed when
     something else moved the pointer mid-drag (the agent reported it, as it should). Give the tests a turn-taking
     lock across projects, or their own virtual desktop
7. [ ] The app's engine is in place (2026-11-11): Start, Pair and Cleanup for both roles as commands, with pairing
   turned round so the master shows the code. Next: the shared window over them (one program per platform that
   asks Master or Worker), the app's icon in it, and the static Linux build as the shipped one.
   - [x] Master's Start for every agent found (2026-11-11): Claude Code, the Claude app, Codex, Antigravity,
     Gemini CLI, Cursor, Windsurf and VS Code each get Master Control added to their own list of MCP servers.
     Tested against a stand-in home folder with each agent's file in its own shape. Open: only Claude Code and
     Codex files were seen on a real machine (this Mac); the others' places come from their documentation,
     and Antigravity's has moved between versions (every place in use is written); a settings file with
     comments in it is left alone and the user told to add the entry by hand. The agents' guide is built (`AGENT-GUIDE.md`, 2026-11-11);
     it has not been tried on an agent other than Claude Code
   - [x] One program holding both roles and the window, on Linux and Windows (2026-11-11): `master-control`
   - [x] The window on macOS and `Master Control.app` with the icon (2026-11-11), opened on the Mac and pictured;
     its buttons were not pressed there (no real input on the Mac), and the worker role can't be installed on
     macOS yet
   - [ ] macOS: the window's colours look a little lighter in its pictures than on Linux and Windows; check on
     the screen itself
   - [x] The window polished (2026-11-11): the icon in it and on the title bar, cards, status pills, digit tiles
     for the code, real bold type, sharp at the display's zoom, no half-drawn frames on X11. Pictures in
     `design/screens/`
   - [x] The static Linux build of the app (2026-11-11): one 10 MB file with no shared libraries. Copies of the
     app are in `/Applications` on the Mac and on the Linux and Windows desktops; a script that builds and
     places all three is still to write
   - [ ] On Windows the master's Pair listens on the network, so Windows Firewall asks about the program; the
     master's Start should add a rule for it, as the worker's does
   - [ ] The worker's Pair by a typed address, for a master not found on the network
   - [ ] A console window flashes when the app opens on Windows
   - [ ] The window's buttons are tested through the screens' own code and tried by hand through the workers;
     there is no automated test that clicks the real window
   - [ ] Cleanup can't remove the program that is running it, on Windows

   Controller, as master and workers (asked 2026-11-11; pairing, sealed calls, scan and the master's MCP server
   built and tested 2026-11-11, and tried live between the Mac and Linux): the master is where the AI agent runs; workers announce
   themselves on the local network and the master lists them. One MCP server on the master covers every worker
   through a `machine` argument. `mctl`, `mctl install`, `mctl test`; the skill switches over to it
   - [x] Pairing by a code (decided 2026-11-11)
   - [x] Install: the worker as a service in the desktop session, with admin rights (2026-11-11). Installed and
     running on Linux (paired with the Mac; screen, input and `elevated` work) and on Windows (task running)
   - [x] Windows reached (2026-11-11): its network is marked Public there, so the firewall rule was widened to
     public networks (his choice); `mc-agent install --any-network` does the same. Paired with the Mac; it runs
     elevated in the desktop session, with screen and input
   - [ ] Install on macOS (LaunchAgent, privileged helper); the install itself has no automated test beyond the
     service and task text: it was proven by installing on the two machines
   - [x] Claude Code on the Mac uses the app's master as its `master-control` MCP server (2026-11-11): installed at
     `~/.local/opt/master-control/mctl`, registered through the shared agent config, and connected
   - [ ] `mctl test` and sending a project's source to a worker; then `scripts/check.sh` and the other projects'
     test scripts can leave the old skill's `mctl`, which keeps that name on PATH until then
   - [ ] An update step: the installed master and workers are copies; rebuilding the project doesn't replace them
   - [x] The master's own machine as a target: `machine: master` (2026-11-11)
   - [ ] Big files in pieces (a call's body is held in memory); announcements
     by mDNS as well as broadcast; `mctl scan` on the Mac may raise the macOS firewall's own question
   - [ ] Not yet tried: a worker on Windows or macOS over the real network; two masters on one network
   - [ ] Install gives the agent admin rights (asked 2026-11-11): a sudoers entry on Linux, a task that runs with
     highest privileges on Windows, a privileged helper with Full Disk Access on macOS. Then `exec` takes
     `elevated`, and tracing, packet capture and attaching to any process need no extra step
8. [ ] Network depth: packets and decrypted HTTPS scoped to the target
9. [ ] Memory depth: snapshots, dumps, watched ranges
10. [ ] macOS agent. Built (2026-11-11): the worker's start-up install and removal, so a Mac can be a worker for
    commands and files; tried for real on the Mac (started by launchd, answered, restarted after a kill, removed
    without trace), on the loopback address only. Not yet tried: a Mac worker reached over the network, which
    needs the macOS firewall's question answered. Control built (2026-11-11): pictures, the window list, focus, pointer, keyboard and
    clipboard. Tried on the Mac, on the app's own window only and with Herbert's yes (2026-11-11): the
    window list, a picture of one window, clicks, typing, a repeated key and Escape, each confirmed by a picture;
    bringing the window forward worked each time. Scroll and right
    click were delivered but have nothing visible to confirm them. Not tried: drag, double click, chords with
    modifiers, the clipboard, raising one window among an app's several, a Retina display. From the command line
    each call is a new process, so positions there are desktop positions, not those of the last picture. The accessibility tree is built too (2026-11-11): `tree`, `find` and `wait_for` read a real AppKit
    window on the Mac (a button, a text field with its value, a label, a ticked checkbox, at their places); that
    test opens a window on the desktop, so it runs only with `MC_MAC_GUI_TESTS=1`. System dialogs (a permission prompt, an alert) are in the window list (0.2.3), and `find`, `wait_for` and
    `press` were tried on a real Contacts prompt: both of its buttons answered without the pointer. There is no
    automated test for that: it needs a real system prompt. Not tried on macOS: the other element actions
    (`set_value`, `toggle`, `expand`, `select`) outside the gated test, and SwiftUI apps. Still to build: several
    displays.
    Observation is built (2026-11-11; 8 tests on the Mac): a started command and everything it starts are
    recorded without administrator rights, from the reports of a sandbox profile that forbids nothing (spec
    §4.3). Open: the live log can lose reports in very heavy bursts and doesn't say so; the stored log
    (`log show`) was more complete in the same trial but ran up to a minute behind, so a second pass over it
    after a session could fill gaps; one early manual run missed the target's first six reads and it didn't
    happen again in the dozens of sessions run since; a target that sandboxes itself can't start inside the reporting sandbox; no addresses, byte counts,
    failed calls or system calls; no crash location (the system's crash report could give it).
    The administrator source is built too (2026-11-11; the same 9 tests run against each source): with sudo
    without a password, processes and files come from `eslogger`, numbered so losses show, and
    `self_sandboxed` watches a program that has its own sandbox. Open there: network still comes from the
    sandbox's reports (ports only, none with `self_sandboxed`); a report that is the only one a process makes
    often never arrives, which is why the full profile is kept; `eslogger` reports the whole machine, so a
    busy machine costs the recorder work (about 2 KB of JSON an event; not measured under a heavy build);
    sessions take about half a second longer to start; its format is not promised by Apple. Root is there now: Herbert installed the sudo rule on the Mac
    (2026-11-11), `exec` with `elevated` answers as root, and `sudo eslogger exec` delivered events from this
    session with no further permission asked. Reading it into sessions is not built
11. [ ] Android bridge: control, then observation
12. [ ] Wayland
13. [ ] Agentless back ends

## Reference study: what is left

Frida, strace, Perfetto and Sysmon for Linux are read from source; Sysmon, Process Monitor, API Monitor and
Instruments from their vendors' pages only.

- [x] Process Monitor run headless on Windows 11 and its export read (2026-11-11). Still open there: why the test
  trace held no network events, and the XML export with stacks.
- [x] API Monitor's definition files and licence read (2026-11-11). Not run: it is a GUI with kernel drivers from
  2013, and its licence forbids decompiling, as Sysinternals' does.
- [ ] Instruments: the full list of instruments, what each records, and what `xctrace` may attach to. Needs Xcode
  on the Mac (only the Command Line Tools are installed).

## Suggestions from the study (not asked for; build only on his yes)

- [ ] Make chosen calls fail on purpose during a test (a full disk, a refused connection, a denied permission), as
  strace does, to check how the app copes.
- [ ] Pause the target at a chosen call and let the agent change its arguments or result, as API Monitor does.

## Versioning and About (2026-11-11)

- The version is `VERSION` (major.minor) plus the commits since that file last changed; `scripts/version.sh` prints
  it and `core/build.rs` builds it in. A build from a copy without git history (what `scripts/check.sh` sends to
  Linux and Windows) gets patch 0 unless `MC_VERSION` is set: the build-and-place script for all three has to pass it.
- The About link points to github.com/dunkelherzmann/master-control, which doesn't exist until the project is
  published.
- Not done: the version in the Windows program's file properties; an About entry in a window menu on Linux and
  Windows (there it is the "About" link in the header); a licence line (there is no licence file yet).
- Before publishing: the older pictures in `design/screens/` were taken on the real machines and may show their
  names and addresses; retake them with `master-control picture`, which shows a stand-in.
- Updating a worker (done by hand 2026-11-11, both now at 0.1.0): send the commit's files, build with
  `MC_VERSION=$(scripts/version.sh)`, then run the new `mc-agent start --name NAME` there (on Windows from the
  desktop session, with `--any-network`); it replaces the program and restarts the worker, and the pairing stays.
  On Windows build over ssh: a build started through the old skill's `mctl exec` said it finished but left the
  old program in the target folder, and the reinstall then put the old program back. Check the built file's time
  or contents before installing; the version number alone doesn't tell two builds of one commit count apart.

## Candidates from the comparison with similar tools (2026-11-11; none approved)

In the order suggested. Each names where the idea was seen (spec §7).

1. [x] Acting on an element by what it is, not where it is (built 2026-11-11: `press`, `set_value`, `toggle`,
   `expand`, `select`). Open: menus of the menu bar aren't in a window's tree, so they can't be reached yet;
   on macOS expand is untried and only AppKit was tried; GTK 4.14 check boxes can't be ticked this way; the
   tree doesn't say whether a box is ticked or a thing expanded, only the action's answer does; an element
   whose name changes by itself (GTK's expander) has to be found afresh before acting.
2. [x] Input on macOS without the pointer or the keyboard (built 2026-11-11 as `background` on `click`,
   `type_text` and `key`, with public calls only). Open: a click posted to a background app as a mouse event is
   ignored by AppKit, so a background click presses or focuses the element at the point instead and can't do
   what only a real click does (place the caret at a spot, drag, right-click, double-click, scroll); Select
   All is done by setting the selection, which always works; any other Command shortcut chooses its menu
   item or falls back to key events, and neither is sure to arrive in a background app (it worked 8 runs in a
   row one hour and failed every run the next); only AppKit was
   tried (not Electron, browsers, SwiftUI or Catalyst; Cua Driver needs private window-server calls for
   those); Windows and Linux don't have it.
3. [x] Several actions in one call (built 2026-11-11 as `batch`). Open: no pause step (use `wait_for`); a
   screenshot inside a batch comes back whole in the answer, which can be large; how much time a batch saves
   over separate calls wasn't measured.
4. [x] Attaching to a running program and filters (built 2026-11-11: `pid`, `classes`, `include`, `exclude`).
   Open: the filters drop events after the recorder has decoded them, so they save storage and reading, not
   the cost of tracing (Linux still stops the target at every system call); a running program's output isn't
   captured (`logs` is empty); on macOS no network for a followed program; what a followed program did before
   the session is unknown, including files it already holds open; by name instead of pid isn't offered;
   on Linux a program already under a debugger can't be followed; Windows runs only a few kernel trace
   sessions at once, so a ninth session waits up to 8 seconds and is then refused.
5. [ ] A call stack with each event. Linux is built (2026-11-11): `stacks: true` gives every event the stack it
   was made from, named from the files' own symbol tables and walked from their unwind tables (the `framehop`
   library), and a crash always carries its backtrace. Windows is built too (2026-11-11): the trace
   session attaches the stack to each event of the process, file and registry providers; library loads tell
   what an address belongs to; names come from export tables and, for the program itself, its PDB file. Open
   there: network and memory events have no stack (they are raised on other threads, or come from the older
   event classes); a crash has no backtrace; a program without its PDB shows offsets; stacks Windows sends in
   two halves are counted in the tracer's log, not joined. macOS (2026-11-11): a crash carries its backtrace, read
   from the report the system writes about it (about two seconds later; the session waits up to four). macOS
   writes no report for a program that has crashed again and again (the first crash gets one, later ones don't
   for a good while), and the problem then says so. What it goes by isn't established: removing the program's
   old reports and the crash reporter's note of its last crash changed nothing, while the same program under a
   new name was reported at once. So the session doesn't rely on
   it: it catches the crash itself (2026-11-11). The recorder sets itself as the handler for fatal crashes,
   which everything it starts inherits; the kernel hands it the crashed thread, its stack is read there and
   then (frame chain, names from the program's symbol table and, for the system's libraries, from this
   process's own copy of them), and the crash is passed on. No admin rights. Open: Apple silicon only; not
   for a program followed by `pid` (nothing was inherited): that falls back to the system's report; frames
   in some system libraries come without a name; a program that changes user on the way loses the handler. Not
   built: per-event stacks on macOS (its sources have none), which refuses `stacks`. Not measured: what stacks cost per event.
6. eBPF as the Linux source, now that root is there: no slowing of the target (Process Monitor for Linux,
   Tracee, Sysmon for Linux).
7. Network content: DNS names, real addresses on macOS, and one program's decrypted traffic through a local
   proxy (Fibratus, mitmproxy's local mode).
8. A published table of which action works on which system and toolkit, proven by small fixture apps (Cua Driver).
9. Wayland (Cua Driver uses libei and the compositors' screen copy).
10. Windows and apps: start and close an app, move, resize, minimize, several displays, notifications.
11. Android, first through plain adb (mobile-mcp), later a pushed server (scrcpy).
12. Smaller ones: a zoom action for dense screens (Anthropic's computer tool), the contents of files a program
    wrote (Tracee), making chosen calls fail to test error paths (strace), per-app permission levels as an
    optional guard.
- [x] A script that builds and places a commit on all three machines and proves it: `scripts/place.sh`
  (2026-11-11).

