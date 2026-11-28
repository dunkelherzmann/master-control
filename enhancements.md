# Enhancements

Every request, in the order received. `- [ ]` pending, `- [x]` shipped (with version and PR).

- [x] 2026-11-11: **Master Control, the app** (Linux, Windows, macOS). An installable app that serves an
  API and MCP endpoint so Claude Code and other coding agents can control a virtual or physical machine, for coding,
  testing and QA of apps written with agentic coders.
  - [x] Linux: the static program, installed as the `master-control` service and paired as a worker
  - [x] Windows: installed as the "Master Control" task with the highest rights, paired as a worker
  - [x] macOS: the master, and a worker too (see "A Mac as a worker")
  - A virtual machine is controlled by the agent installed inside it; control through the hypervisor with no
    agent inside is a separate question (`pending.md`, agentless back ends)
- [ ] 2026-11-11: **See and drive the machine** (all platforms). The agent sees the monitor and controls the keyboard
  and mouse as if a user sat in front of the computer. Spec §3.
  - [ ] Linux: screen, pointer, keyboard, clipboard and the accessibility tree on X11 built and tested
    (2026-11-11); GNOME on Wayland built and tested (2026-11-16); other Wayland desktops still to come
  - [ ] Windows: screen, pointer, keyboard, clipboard and the accessibility tree built and tested (2026-11-11);
    window management, app launch and virtual desktops built too (#25); recording still to come
  - [x] macOS: screen, pointer, keyboard, clipboard and the accessibility tree built and tested on the master's
    own machine (2026-11-11; in 0.2)
- [ ] 2026-11-11: **Watch everything the target app does** (all platforms). Constantly monitor, check, scan and trace
  the app's kernel calls, reads and writes, memory, storage, inbound and outbound network, logs, the Windows
  registry, and everything else it does in the foreground and background, so the agent can code and troubleshoot on
  the fly. Spec §4.
  - [ ] Linux: a started command's processes, files, network, memory, system calls and crashes are recorded and
    searchable, with guards and a verdict (2026-11-11); attaching by pid and call stacks built since; attaching
    by app name, eBPF, memory watching and packets still to come
  - [ ] Windows: a started command's processes, files, registry changes, network and crashes are recorded and
    searchable, with guards and a verdict (2026-11-11); attaching by pid and call stacks built since; memory,
    system calls and the place of a crash still to come
  - [ ] macOS: a started command's processes, files, network ports, services looked up and crashes are recorded and
    searchable, with guards and a verdict, without administrator rights (2026-11-11); addresses, byte counts,
    system calls still to come; attaching built since, with admin rights. With admin rights (2026-11-11)
    processes and files come from the system's own events, with arguments, renames and a count of lost events,
    and a program with its own sandbox can be watched (`self_sandboxed`)
- [x] 2026-11-11: **Study similar apps** with craft-and-rea to get the spec information: apps that cover the
  functionality partly or in full. Spec §7; evidence in `.investigation/` (untracked).
- [x] 2026-11-11: **Trackers on disk**: `bugs.md`, `enhancements.md`, `pending.md` and `specs.md` in the project
  root, tracking everything seen and done.
- [x] 2026-11-11: **Rust core**: the coding core is Rust, unless a better choice turns up. Recorded in `pending.md`.
- [x] 2026-11-11: **Local first**: work locally; commit to GitHub only once a functional and tested prototype is ready.
  Kept until 2026-11-12, when the project was published at 0.2.
- [x] 2026-11-11: **Study the tools left out of the first pass** with craft-and-rea: Frida, strace, Perfetto, Sysmon,
  Process Monitor, API Monitor, Instruments. Spec §4 and §7. Seeing Process Monitor, API Monitor and Instruments in
  use is still open in `pending.md`.
- [x] 2026-11-11: **Download Process Monitor and API Monitor** to Windows and study them in use. Process Monitor was
  run and its trace read; API Monitor was read from its files, not run. Spec §4.5 and §7.
- [x] 2026-11-11: **Admin rights for Master Control** (all platforms). Master Control is added to sudoers or given admin
  access, and anything it needs to run has full permissions where required, so that tools needing elevation or sudo
  just work. Spec §6.
  - [x] Linux: installed as a service; `elevated` commands run as root through sudo (2026-11-11)
  - [x] Windows: installed as a task with highest privileges (2026-11-11)
  - [x] macOS: a sudo rule Herbert installed himself (`/etc/sudoers.d/master-control`, 2026-11-11); an `elevated`
    command through the master answered as root
- [x] 2026-11-11: **Windows equivalents of the Linux-only observation** (Windows), worked out with craft-and-rea from
  the Linux tracer, Fibratus and the Windows machine's event layouts: command lines, memory events, results and
  real byte counts of file operations. What Windows can't tell is listed in `pending.md`. Spec §4.3.
- [x] 2026-11-11: **Master and workers** (all platforms). The controller works as master and workers: the master is the
  one machine the AI agent runs on and is signed in to (this Mac); each worker finds its way to the master over the
  local network and makes itself available to it. Claude, Codex or Antigravity is run and signed in on the master
  only, and its agents still work on the workers as they do now, with fewer hoops. Before the controller is built.
  Paired by a code (his choice, 2026-11-11). Pairing, sealed calls, scan and the master's MCP server are built and
  tested on all three (2026-11-11). Workers are installed and paired on Linux and Windows, and Claude Code on the
  Mac is connected to the master (2026-11-11).
  - [x] Linux: worker installed, paired
  - [x] Windows: worker installed, paired
  - [x] macOS: a macOS worker and its install are built and tested (see "A Mac as a worker"); not installed
    here, since this Mac is the master
- [x] 2026-11-11: **The master controls itself too** (all platforms). The master exposes the same access to its own machine
  as to a worker, on top of coordinating and controlling the workers: master = full control of itself and of its
  workers; worker = full control of itself by its master.
  - [x] The same tools on the master's own machine, as `machine: "master"` (2026-11-11)
  - [x] Screen, input, the accessibility tree and observation on a macOS master (2026-11-11; in 0.2)
- [x] 2026-11-11: **An app for Master Control** (all platforms), with an original icon: the agentic-brain idea inside a
  circle on a rounded square, in the colours of the macOS Apps icon. Several icon candidates first, before moving on.
  - [x] Icon chosen: candidate D, the ring of six colours (2026-11-11); it is `design/icon.svg`
  - [x] macOS: the `.app`'s `AppIcon.icns` (`macos/scripts/build.sh`)
  - [x] Linux: the window's `_NET_WM_ICON` (`app/src/window_x11.rs`)
  - [x] Windows: the window's icon (`app/src/window_win.rs`)
- [x] 2026-11-11: **The app: one self-contained program per platform that sets a machine up as Master or Worker** (all
  platforms). One `.app`, one `.exe`, a Flatpak for Linux. On first start it asks: run as Master or as Worker.
  - Master: asks for elevation (or runs through sudo); installs everything the AI agents need (skills, tools, MCP
    and so on) together with worker-level tools and access; then generates a pairing code. Back, Cancel and Done
    buttons.
  - Worker: asks for elevation (or runs through sudo); installs everything needed to connect to the master for AI
    agent access and coordination; then asks for a pairing code. Back, Cancel and Done buttons.
  - Started again on a machine where it is installed, it offers to uninstall and delete every component, skill,
    tool, MCP entry and the rest.
  - Replaced the same day by the correction below, which drops the first-start wizard.
- [x] 2026-11-11: **Correction to the app** (all platforms; replaces the first-start wizard of the entry above). Master
  Control stays one self-contained program per platform. Running it asks for permissions or elevation, then offers:
  - Master: 1. Run (installs the skills, tools, MCP and the rest if they aren't installed). 2. Pair (generates a
    random pairing code). 3. Cleanup (removes everything Run installed).
  - Worker: 1. Run (installs the tools, MCP and the rest if they aren't installed). 2. Pair (asks for the pairing
    code from the master). 3. Cleanup (removes everything Run installed).
  - Formats: `.app`, `.exe`, and for Linux whatever binary format gives a portable, self-contained program that
    needs nothing from the OS.
  - [x] The engine: Start, Pair and Cleanup for both roles as commands, pairing with the master's code
    (2026-11-11; tested on all three, installed on the Mac, Linux and Windows)
  - [x] The shared window, drawn by the program itself (his choice, 2026-11-11): built for Linux, Windows and
    macOS, with the icon (2026-11-11)
  - [x] macOS: `dist/Master Control.app` (`macos/scripts/build.sh`), in `/Applications`
  - [x] Windows: one `.exe` that asks for elevation, on the desktop
  - [x] Linux: one static musl program that needs nothing from the system, on the desktop; admin steps go
    through pkexec or sudo
- [x] 2026-11-11: **Make the Windows and Linux apps look finished** (Linux, Windows): they looked like unfinished work
  with no polish. Redrawn with the icon, cards, status pills, digit tiles, real bold type and sharp rendering at
  the display's zoom. Pictures in `design/screens/`.
- [x] 2026-11-11: **Identify every machine by host name + OS release + MAC address** (all platforms): each computer
  running Master Control, master or worker, is told apart by the combination of the three. Built for Linux,
  Windows and macOS (2026-11-11). Spec §5.
- [x] 2026-11-11: **A Mac as a worker** (macOS). Asked after "why can't Macs be workers yet?".
  - [x] Commands and files: the worker's start-up install and removal (2026-11-11)
  - [x] Screen, keyboard, mouse and the accessibility tree: pictures, windows, pointer, keyboard and clipboard
    built (2026-11-11); clicks, typing and keys verified on the Mac on the app's own window; the accessibility
    tree built and read from a real AppKit window (2026-11-11)
  - [x] Observation: a started command and everything it starts: programs, files, network ports, services,
    exits and crashes, with guards and a verdict, without administrator rights (2026-11-11). Less detail than
    Linux and Windows; what is missing is in `pending.md`
- [x] 2026-11-11: **Versioning and an About panel** (all platforms): add versioning to Master Control and show its
  info the way Herb's Terminal's About window does: the icon, the name, "Version 1.0.245 (1.0.245)", a one-line
  description, "By dunkelherzmann" and the link to the GitHub project.
  Built (2026-11-11): a `VERSION` file (0.1) and `scripts/version.sh`, as in Herb's Terminal; every program and
  `status` report the version; an About screen in the shared window, opened from "About" next to the version
  and, on macOS, from the app's menu. Picture: `design/screens/about.png`.
  - [x] macOS: built and placed on the Mac
  - [x] Linux: worker and the app on the desktop replaced, both report 0.1.5 (2026-11-11); the About screen
    passes its tests there but hasn't been opened by hand on that machine
  - [x] Windows: worker and the app on the desktop replaced, both report 0.1.5 (2026-11-11); the About screen
    passes its tests there but hasn't been opened by hand on that machine
- [x] 2026-11-11: **Compare Master Control with similar solutions**: what it does worse, the same and better,
  and what it could adopt. Answered in chat from the tools studied in spec §7; the candidates to adopt are listed
  in `pending.md`, none approved yet.
- [x] 2026-11-11: **Act on elements by what they are** (all platforms; the first candidate from the comparison,
  approved). Five tools (`press`, `set_value`, `toggle`, `expand`, `select`) that work on an element id from
  `tree` or `find` without the pointer or the keyboard. Spec §3.
  - [x] macOS: press, set a text, tick and untick, select a radio button verified on a real AppKit window;
    expand not tried there
  - [x] Linux: all five verified on a real GTK 4 window (a switch for toggle; GTK's check box can't be ticked
    this way and says so)
  - [x] Windows: all five verified on Win32 controls (button, edit, check box, drop-down list)
- [x] 2026-11-11: **Input on the Mac that doesn't take the pointer or the keyboard** (macOS; the second candidate
  from the comparison, approved). `click`, `type_text` and `key` take `background: true`: nothing comes to the
  front, the pointer stays where it is, and the real keyboard isn't used. Verified on a real AppKit window that
  stayed behind throughout: a field given the keyboard, text typed, a key repeated, Cmd+A, a button pressed.
  Spec §3.
- [x] 2026-11-11: **Several actions in one call** (all platforms; the third candidate from the comparison,
  approved). A `batch` tool runs up to 30 steps in order and lets a step use what an earlier one answered.
  Tested on all three systems, through the master to a worker, and on a Mac window (find, press, wait in one
  call). Spec §3.
- [x] 2026-11-11: **Follow a program that is already running, and filter what is recorded** (all platforms;
  the fourth candidate from the comparison, approved). `observe_start` takes `pid`, and `classes`, `include` and
  `exclude`. Spec §4.1.
  - [x] Linux: followed through sudo (this machine's kernel only lets a parent trace), to its end and let go
    on stop; filters
  - [x] Windows: followed to its end and let go on stop; filters
  - [x] macOS: followed with admin rights, to its end and let go on stop; refused without; filters on both
    sources
- [x] 2026-11-11: **A call stack with each event** (all platforms; the fifth candidate from the comparison,
  approved).
  - [x] Linux: every event with `stacks: true`, and every crash, carries its named call stack
  - [x] Windows: file, registry and process events with `stacks: true` carry it, the program's own functions
    named from its PDB (2026-11-11); crashes don't yet
  - [x] macOS: every crash of a program the session started carries its backtrace, caught as it happens
    (2026-11-11); per-event stacks aren't available from macOS's sources
- [x] 2026-11-11: **The app works with every agent, GUI or CLI** (all platforms): find out why the app can't
  install on this machine and can't detect the Claude Code app and the Codex app; make it compatible with the
  GUI apps and the CLIs of Claude, Codex and Antigravity, and any other agentic AI app or CLI. Start now finds
  agents by their settings folder, their app or their command wherever installers put it, and registers Master
  Control with each in its own file; `mctl agents` lists them; the app's Start says which are set up. Spec §6.
- [x] 2026-11-11: **A helper file for other coding agents** (all platforms): a file third-party agentic coders
  can read to understand how to integrate and use Master Control themselves. `AGENT-GUIDE.md` at the project's
  root, carried inside the program (`mctl guide`, the MCP tool `guide`) and left beside the installed program
  by Start. Also asked: run Start on the Mac: done; Claude Code, the Claude app and Codex have Master Control.
- [x] 2026-11-11: **More legible text, and a live status section** (all platforms): increase the font size
  without changing the size of the UI's elements (the point being text that is easier to read than it was),
  and add a status section under Master and Worker to watch what is happening live. Text is drawn 16% larger
  in the same boxes, the two greys are lighter, and letters are drawn a little fuller on the dark ground. The
  first screen has a Status panel: the master and the agents it is set up for, each worker and whether it
  answers, this machine's own worker, and the master's latest calls (machine, tool, result, time) as they
  happen. Pictures: `design/screens/choose.png`, `master.png`.
- [x] 2026-11-11: **The status says this machine is the master** (all platforms; his correction): running a
  machine as master lets agents use it to the full extent a worker would allow, so saying it isn't a worker is
  redundant. The line reads "This machine is the master, for …"; the worker role is named only where it is
  the one in use.
- [x] 2026-11-11: **Status lights that blink with traffic** (all platforms): the green indicators flicker like
  a disk light when there is inbound or outbound activity. A worker's light flickers while the master has a
  call to it under way (and for a moment after); this machine's flickers with any call passing through it, or,
  on a worker, with requests coming in from its master.

- [x] 2026-11-12: **Publish the project on GitHub as an open repo** (all platforms): a public repository with
  nothing private in it and text that reads as written by a person. Done: machine names, LAN addresses and the
  pictures of real desktops are out of the tracked files (the check and install scripts read the machines' ssh
  names from an untracked `scripts/hosts.env`); `README.md`, the MIT `LICENSE` and `NOTICES` are new; the guide,
  the spec and the pending list lost their long dashes. The public history starts fresh at 0.2 (his choice): the
  earlier commits held pictures of real desktops and stay on his Mac.

- [x] 2026-11-12: **Plain wording in the explanatory docs** (all platforms): a pass with the humanizer skill over
  `README.md`, `AGENT-GUIDE.md` and `specs.md`. In #1: one long dash in the spec's macOS observation row became a
  sentence break, and the licence line lost a filler word. The rest already read plainly.

- [x] 2026-11-12: **System dialogs in the window list** (macOS): the dialogs macOS shows for an app (a permission
  prompt, an alert) appear in `windows`, titled with their question, so `tree`, `find`, `wait_for` and `press`
  work on them and an agent can answer one without moving the pointer. Linux and Windows already list their
  dialogs as ordinary windows. In 0.2.3, #2.

- [x] 2026-11-12: **Publish the builds as a release** (all platforms): release 0.2.3 (tag `v0.2.3`) holds the
  builds already on the three machines, all made from that commit: the Mac app as a zip, the static Linux program
  as a tar.gz and the Windows program as a zip, with checksums. First published as a pre-release, then made a
  full release marked Latest the same day, so the repository's front page shows it.

- [x] 2026-11-12: **Plain wording in the trackers** (all platforms): a second pass with the humanizer skill, this
  time over `bugs.md` and `enhancements.md`. Their long dashes are gone: dates and platform names are followed by a
  colon. No entry was removed or reordered.

- [x] 2026-11-20: **Faster, lighter screenshots that keep clicks on target** (all platforms), from a codigus pass
  over Master Control. A screenshot returned to an agent is kept to a 1080p screen's worth of pixels (at most 2576 on
  a side): Claude's models shrink anything larger, after which their positions no longer match the picture's pixels
  (the Mac's 3440x1440 and Windows' 2560x1600 screens were over the limit). `full_size` keeps every pixel. PNGs are
  written with fast compression, about a tenth of the time. Shipped in 0.2.16 (#16): a default screenshot now takes 211 ms on the Mac (was 293), 618 ms on
  Linux (was 929) and 229 ms on Windows (was 882).

- [x] 2026-11-20: **No Android** (all platforms): Android is removed entirely: we won't work on it now nor in the
  near future. Taken out of the spec, the trackers, the pending list and the README (0.2.19, #19).

- [x] 2026-11-20: **One universal Linux program** (Linux): the Linux package stays one self-contained, runnable
  binary that needs no installing, and when run and set up it installs what the system needs. It decides by itself
  whether the session is Xorg or Wayland, all inside the same binary.
  Shipped in 0.2.19 (#19) and 0.2.20 (#20): the static program is the window, the master and the worker (the Linux worker now runs
  from it); Start, or `master-control agent setup` alone, installs the accessibility bus, sudo and gsettings through
  apt, dnf, pacman or zypper, and the sudo rule, asking for the password once (sudo, the desktop's prompt, or the
  terminal). Tried in a fresh Ubuntu 24.04 container as root, as a user without sudo and with a password-asking sudo.
  Not tried by hand: the desktop's password prompt (pkexec).

- [x] 2026-11-26: **Scripts run commands on the Linux and Windows machines through Master Control (mc-exec) instead of SSH** (developer tooling): `scripts/check.sh`, `scripts/place.sh` and `scripts/gen-syscalls.sh` copy files with `mc-exec --put` and run through `mc-exec`, by the Master Control names `linux` and `windows`; `place.sh` starts the worker replacement outside the worker (a systemd unit, a scheduled task) and waits for the new one to answer. Shipped in #22.

- [x] 2026-11-26: **`mc-exec` ships with Master Control** (developer tooling): `scripts/mc-exec` runs a command on a machine through `mctl` with its own output and exit code, and copies files of any size (`--put`/`--get`), so the repository's scripts need nothing beyond the master's `mctl` (asked 2026-11-26). Shipped in #23.

- [x] 2026-11-26: **Close the gaps found against Windows-Use** (all platforms; asked after the window-control
  comparison: "add them and build them, see if they can be implemented on Linux and Mac also").
  - [x] Click with keys held (`hold` on the clicks)
    - [x] Linux (X11), Windows
    - [x] Mac: tested 2026-11-27 after the take-over pop-up let it in: a test window saw each click with Command,
      Shift, Option, Control and Command-Shift held, and nothing stayed held after
  - [x] Typing helpers (`clear`, `caret`, `enter` on `type_text`): tested on the Mac (in the background), Linux
    (GTK 4) and Windows
  - [x] A compact view (`interactive` on `tree` and `find`): tested on the Mac, Linux and Windows
  - [x] Move, resize, minimize, maximize and restore a window (`arrange`), shipped in 0.2.42 (#25)
    - [x] Mac, Windows, Linux on X11 (moving and sizing; states need a window manager, which the test display
      lacks)
    - [x] Linux on GNOME: placed, minimized, restored, maximized and placed back in the signed-in session
  - [x] Launch an app by its name, and list the apps (`apps`, `launch`): tested on the Mac, Linux and Windows;
    shipped in 0.2.42 (#25)
  - [x] Virtual desktops (`desktops`)
    - [x] Windows: list, switch, create, remove (Explorer's own shortcuts)
    - [x] Mac: list, and switch with the system's Control-arrow shortcuts, tested 2026-11-27 (Space 1 to 2 and
      back, 1 to 3 and back) once the arrows carried Fn (#33); macOS offers no way to add or remove Spaces
    - [x] Linux on GNOME: list and switch tested in the signed-in session; create and remove built, not tested;
      X11 needs a window manager
  - [x] Faster window reading (UI Automation cache, one call a window) and a stronger bring-to-front on Windows

- [x] 2026-11-27: **Ask before taking over a computer someone is using** (macOS, Linux, Windows; asked 2026-11-27:
  "whenever the coding/ai agent detects via master control that the user is currently using the computer … create
  a pop up window letting the user know that you need to take over and give them an ok button … if the user clicks
  cancel you record the decision, acknowledge the negative and explain why you can't do the relevant action/test").
  Before the pointer, the keyboard or bringing a window forward, if the machine had real input in the last minute,
  a native pop-up asks with OK and Cancel and no timer; OK lets the agent's session carry on, Cancel refuses those
  actions (`user_declined`) and is recorded; a pop-up says when the machine is the user's again. Shipped in #28
  (tool `take_over`).
  - [x] Mac: OK let the session in; Cancel was recorded and refused the next steps with `user_declined`
  - [x] Linux (GNOME): the same, through the shell extension (API 4)
  - [x] Windows: the same; real input is told from the agent's and tests' own, and the pop-up is brought forward
    over a full-screen browser

- [x] 2026-11-27: **A pop-up says when the computer is the person's again** (macOS, Linux, Windows; asked 2026-11-27:
  "Create a companion pop up letting the user know that they can start using the computer again"). When an agent
  that was let in is done, a pop-up with one OK button tells the person they can use the computer again, in place of
  a notification; the agent doesn't wait for it to be clicked. A question closes one still open, and none is shown
  while a question waits, so they never cover each other. Shipped in #28.
  - [x] Mac: shown on release, and closed by the next question
  - [x] Linux (GNOME): shown on release
  - [x] Windows: shown on release, in front of the window in use

- [x] 2026-11-27: **The take-over question ends with the agent's reason** (macOS, Linux, Windows; asked 2026-11-27:
  "add 'Click ok/cancel on this window' to your test windows at the end of the pop-up text so that I can do the
  expected action"). The reason the agent gives is the pop-up's last line, so a test's "Click Cancel on this
  window." is what the person reads last. Shipped in #28.

- [ ] 2026-11-27 (Mac, Linux, Windows): Move to one shared Rust core and the same egui window on all three systems, with the Mac version as the reference: everything shared where it can be, and what egui can't do on a platform done with that platform's own code for the same feature; each version built and tested on its own computer through Master Control.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-27 (Mac, Linux, Windows): The platform rules: each system ships a self-contained installable variant and a portable binary (Mac: the `.app`; Linux: a Flatpak and one static binary; Windows: a zip with an install script that gets past SmartScreen and the missing signature, explained with its caveats, beside the `.exe` installer that installs to Program Files with an uninstaller `.exe` and an entry in installed apps, plus the portable `.exe`); the app follows the system's light or dark mode in each platform's own Finder, File Explorer or window-manager colours; and on top of the feature tests, a side-by-side check against the Mac with pictures of every window.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [x] 2026-11-27 (Mac): Security and dependency checks in scripts/check.sh: gitleaks on the history, cargo audit on the dependencies (PR #35)

- [ ] 2026-11-27: Local crash reports: when the app crashes it saves a report (what happened, the version, the stack) in its own data folder on that machine, never sent anywhere, and shows it at the next start so the agent can read it.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [x] 2026-11-28 (Mac, Linux, Windows): **Hear the machines.** Let the master "hear" sounds coming from the master
  and the workers when needed, so an agent can check a bell, an alert or any app's sound. His idea: create or
  temporarily plug in a virtual sound card that routes audio to the master doing the listening; it doesn't have to
  be done that way. First use: Herb's Terminal's bell (feature test B6), which its suite can't check today.
  Proposed, to decide when it is built: capture what the machine plays without a driver where the system offers it
  (macOS ScreenCaptureKit audio, Linux the PipeWire/PulseAudio monitor of the default output, Windows WASAPI
  loopback, each optionally for one app only), with a virtual sink only where there is no output device (a headless
  session); return a recording the master can pull and play, plus what an agent can test without listening (sound
  started and stopped at, loudness, and a `wait_for` on a sound), recorded with the session's other events.
  Built 2026-11-28 (#41): `listen_start` and `listen_stop` (spec §3.5), with no virtual sound card needed on any of
  the three; one app's sound only is left for later.
  - [x] Mac: ScreenCaptureKit audio
  - [x] Linux: PipeWire's monitor of the default output
  - [x] Windows: WASAPI loopback

- [x] 2026-11-28 (Mac, Linux, Windows): **A password vault for sudo** (his request, to be evaluated by the Sage
  first): a vault where he enters the sudo password of each machine, master and workers, which Master Control uses
  without the coding agent ever knowing them. He adds that Master Control is meant to have passwordless sudo or admin
  rights so it needs no passwords at all: "Master" Control gives the coding agent unrestricted access to every host.
  **The Sage's evaluation (2026-11-28): not recommended as a vault.** Unrestricted access is already in place without
  passwords: `sudo -n` works on the Mac (`/etc/sudoers.d/master-control`) and on Linux (`NOPASSWD: ALL`), and the
  Windows worker runs elevated (High Mandatory Level), so `exec` with `elevated` needs no password anywhere. A vault
  would keep root passwords on disk for no gain there, and an agent may not type a real password through it unless
  he approves each use where it is stored. The gap that remains is the system's own password dialogs, which sudo
  doesn't cover: polkit on Linux (no rule in `/etc/polkit-1/rules.d`, so pkexec and GNOME Settings still ask) and
  macOS's authorization dialogs. Proposed instead, waiting for his yes:
  - [x] Linux: Start installs a polkit rule that lets this user do every admin action without asking, beside the
    sudo rule, and Cleanup removes it; `status` says whether both are in place (approved 2026-11-28, #42)
  - [x] macOS: Start checks the sudo rule and says how to add it when it's missing (it was put in by hand); the
    system's authorization dialogs stay the person's, since synthetic input can't type into them (approved
    2026-11-28, #42: the master's Start checks it now, as the worker's did)
  - [x] Windows: nothing to add (the elevated worker starts programs elevated, so no UAC prompt appears); `status`
    says so (#42; installed 0.2.95 on all three, 2026-11-28: Linux pkcheck went from auth_admin to yes)

- [ ] 2026-11-28 (Mac, Linux, Windows): **Decision for Herbert: the name.** "Master Control" is tentative. From `pending.md`, open decisions.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Decision for Herbert: agentless back ends.** Should VM control through the hypervisor and physical machines through a KVM-over-IP device be in the first version, or come after the four agents work? From `pending.md`, open decisions.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac): **Probe: Endpoint Security events for a tool of our own.** Which events an unpaid-signed tool of ours could receive beyond what stock `eslogger` as root gives (that source is built, spec 4.3), and what System Integrity Protection blocks beyond `self_sandboxed`. From `pending.md`, to prove before promising.
  - [ ] Mac

- [ ] 2026-11-28 (Mac, Linux, Windows): **Probe: whole-program memory tracing.** Whether whole-program read and write tracing is usable anywhere, or only watched ranges. Spec 4.2. From `pending.md`, to prove.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Probe: ETW system-call flag.** What the ETW system-call flag gives on Windows 11, and whether API hooking is needed for arguments. Spec 4.3. From `pending.md`, to prove.
  - [ ] Windows

- [ ] 2026-11-28 (Linux): **Probe: Wayland desktops other than GNOME.** KDE Plasma and wlroots compositors: screen capture and input without a prompt on every run (GNOME is built through our own shell extension). Spec 3.3. From `pending.md`, to prove.
  - [ ] Linux

- [ ] 2026-11-28 (Mac, Linux, Windows): **Probe: which hypervisors first.** What each hypervisor exposes for display and input. Spec 2. From `pending.md`, to prove.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Probe: newer MCP protocol revision.** The agent and the master speak protocol versions 2024-11-05 to 2025-06-18 (Claude Code connects, checked 2026-11-11); the newer revision is still to read. Spec 5. From `pending.md`, to prove.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Control: close windows, install and stop apps, record the screen.** `close_window`, `install`, `stop`, `record_start`/`record_stop`. From `pending.md`, build order step 3.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Linux): **Linux control tests under a window manager, several monitors, HiDPI.** Focus through the window manager is only smoke-tested in GNOME, by listing windows. From `pending.md`, build order step 3.
  - [ ] Linux

- [ ] 2026-11-28 (Windows): **Windows control: higher-rights window in front, mixed-scaling monitors, windows GDI can't capture.** The refusal for a window with higher rights in front is written, not tested; several monitors with different scaling; pictures of windows GDI can't capture. From `pending.md`, build order step 3.
  - [ ] Windows

- [ ] 2026-11-28 (Linux, Windows): **Test `focus_refused`.** Neither platform has a test: nothing in the test set-up refuses focus. From `pending.md`, build order step 3.
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Linux): **Accessibility: try GTK 3, Qt, Electron and Chromium apps on Linux.** Linux is tested with one GTK 4 app (zenity) only. From `pending.md`, build order step 4.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **GTK 4 places of containers leave out padding.** GTK 4 reports places that leave out its containers' padding (about 30 pixels in libadwaita dialogs). A plain click by id no longer uses them (`bugs.md`, 2026-11-16); double and right clicks, drags and scrolling by id still do, and the places in `tree` and `find` are still GTK's. From `pending.md`, build order step 4.
  - [ ] Linux

- [ ] 2026-11-28 (Windows): **Accessibility: try WPF, WinUI, WebView2 and Electron apps on Windows.** Windows is tested with classic Win32 controls only. From `pending.md`, build order step 4.
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux): **Faster large accessibility trees on Linux (AT-SPI) and macOS.** They still ask one property at a time; Windows reads a window in one cached call (#25). From `pending.md`, build order step 4.
  - [ ] Mac
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: eBPF source.** A root-helper source for speed and for attaching to anything; decide the toolchain (aya or libbpf). From `pending.md`, build order step 5 and comparison candidate 6 (no slowing of the target).
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: attach to a running process by app name.** Attach by pid is built. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: measure how much the tracer slows a target.** Not measured yet. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: ARM64.** The tracer reads x86-64 registers and call numbers. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Linux, Windows): **Observation: `stream`, `snapshot`, `stack`, `dump`, `watch_memory`, `capture_content`.** Not built on Linux; on Windows `stream`, `snapshot` and the rest, as on Linux. From `pending.md`, build order steps 5 and 6.
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Linux): **Observation on Linux: baselines and limits.** Memory growth, descriptors, CPU, run time; hangs. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: DNS names, packets, decrypted HTTPS.** DNS names for addresses; packets; decrypted HTTPS (network depth, build order step 8). Related: a program's decrypted traffic through a local proxy (comparison candidate 7). From `pending.md`.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Observation on Linux: settings, ipc, ui and log events.** Settings (dconf, config files as their own class), ipc, ui and log events; the system log. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Linux): **Delete a session and its files; read sessions from before an agent restart.** Sessions from before an agent restart can't be read by the tools. From `pending.md`, build order step 5.
  - [ ] Linux

- [ ] 2026-11-28 (Windows): **Observation on Windows: session end delay.** A session ends about two seconds after its target does, waiting for the trace buffers; events arrive up to a second late. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Observation on Windows: count events lost to trace buffer overflow.** Not counted yet. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Observation on Windows: name a registry key opened before the session started.** A key opened before the session (an inherited handle) is named only from that key down. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Observation on Windows: still behind Linux.** Opens don't say read or write (Windows doesn't report it); no memory protection changes or file mappings; no system calls (not available by name from ETW as read; to prove another way); no thread or image events. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Observation on Windows: where a crash happened, hangs, error dialogs.** A dump from Windows Error Reporting for the place of a crash. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **A turn-taking lock for the shared Windows desktop.** The machine's desktop is shared with other projects' GUI tests: one run in four failed when something else moved the pointer mid-drag (the agent reported it, as it should). Give the tests a turn-taking lock across projects, or their own virtual desktop. From `pending.md`, build order step 6.
  - [ ] Windows

- [ ] 2026-11-28 (Mac): **macOS: check the window's colours on the screen itself.** They look a little lighter in the window's pictures than on Linux and Windows. From `pending.md`, build order step 7.
  - [ ] Mac

- [ ] 2026-11-28 (Mac, Linux, Windows): **The worker's Pair by a typed address in the window.** The command does it (`mc-agent pair CODE ADDRESS`) for a master not found on the network; the window's Pair screen has no field for it yet. From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **A console window flashes when the app opens on Windows.** From `pending.md`, build order step 7.
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **An automated test that clicks the real window.** The buttons are tested through the screens' own code and tried by hand through the workers. From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Cleanup can't remove the program that is running it, on Windows.** From `pending.md`, build order step 7.
  - [ ] Windows

- [ ] 2026-11-28 (Mac): **Install the worker on macOS with a proper helper.** LaunchAgent and privileged helper; the install has no automated test beyond the service and task text (proven by installing on two machines). From `pending.md`, build order step 7.
  - [ ] Mac

- [ ] 2026-11-28 (Mac, Linux, Windows): **`mctl test` and sending a project's source to a worker.** So `scripts/check.sh` and the other projects' test scripts can leave the old skill's `mctl`, which keeps that name on PATH until then. From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **An update step for installed masters and workers.** They are copies; rebuilding the project doesn't replace them (`scripts/place.sh` does it by hand). From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Big files in pieces, mDNS announcements, firewall question for `mctl scan`.** A call's body is held in memory; announcements by mDNS as well as broadcast; `mctl scan` on the Mac may raise the macOS firewall's own question. From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Windows): **Try a worker on Windows or macOS over the real network, and two masters on one network.** Not yet tried. From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Try the agents' guide on an agent other than Claude Code.** `AGENT-GUIDE.md` is built; only Claude Code and Codex files were seen on a real machine (the other agents' places come from their documentation; Antigravity's has moved between versions). From `pending.md`, build order step 7.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Network depth: packets and decrypted HTTPS scoped to the target.** From `pending.md`, build order step 8.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Memory depth: snapshots, dumps, watched ranges.** From `pending.md`, build order step 9.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac): **macOS agent: what is not tried or built yet.** Control: drag, double click, modifier chords, the clipboard, raising one window among an app's several, a Retina display, several displays; element actions (`set_value`, `toggle`, `expand`, `select`) outside the gated test, SwiftUI apps; a Mac worker reached over the network (needs the macOS firewall's question answered). From `pending.md`, build order step 10.
  - [ ] Mac

- [ ] 2026-11-28 (Mac): **macOS observation: gaps and costs.** Live log can lose reports in heavy bursts without saying so (the stored `log show` is more complete but up to a minute behind: a second pass could fill gaps); a target that sandboxes itself can't start inside the reporting sandbox; no addresses, byte counts, failed calls, system calls or crash location; with admin rights network still comes from sandbox reports (ports only), a lone report from a process often never arrives, `eslogger` reports the whole machine (cost under a heavy build not measured), sessions start about half a second slower, Apple doesn't promise its format. Reading `sudo eslogger exec` into sessions is not built. From `pending.md`, build order step 10.
  - [ ] Mac

- [ ] 2026-11-28 (Mac, Linux, Windows): **Agentless back ends.** VMs through the hypervisor, physical machines through a KVM-over-IP device (Spec 2; see the decision entry above). From `pending.md`, build order step 12.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Windows): **Reference study: why Process Monitor's test trace held no network events.** And the XML export with stacks. From `pending.md`.
  - [ ] Windows

- [ ] 2026-11-28 (Mac): **Reference study: Instruments.** The full list of instruments, what each records, and what `xctrace` may attach to. Needs Xcode on the Mac (only the Command Line Tools are installed). From `pending.md`.
  - [ ] Mac

- [ ] 2026-11-28 (Mac, Linux, Windows): **Suggestion, awaiting his yes: make chosen calls fail on purpose during a test.** A full disk, a refused connection, a denied permission, as strace does, to check how the app copes. From `pending.md`.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Suggestion, awaiting his yes: pause the target at a chosen call.** Let the agent change its arguments or result, as API Monitor does. From `pending.md`.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Linux): **Suggestion, awaiting his yes: faster Linux screenshots.** GNOME Shell spends about 0.57 s of each one writing a PNG for our extension (measured 2026-11-20). Handing over raw pixels would need a changed extension, which loads at sign-in. From `pending.md`.
  - [ ] Linux

- [ ] 2026-11-28 (Windows): **Windows file properties and About entries.** The version in the Windows program's file properties; an About entry in a window menu on Linux and Windows (there it is the "About" link in the header); a licence line in the About screen (the `LICENSE` file is there since 0.2). From `pending.md`, versioning and About.
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Comparison candidates still open.** Menus of the menu bar aren't in a window's tree; macOS expand untried and only AppKit tried; GTK 4.14 check boxes can't be ticked by element action; the tree doesn't say whether a box is ticked; background input on macOS is limited (see `enhancements.md`, input without the pointer; Windows and Linux don't have it); a published table of which action works on which system and toolkit, proven by small fixture apps (Cua Driver); a zoom action for dense screens, file contents a program wrote (Tracee), per-app permission levels as an optional guard; per-event stacks on macOS (its sources have none) and what stacks cost per event; closing windows, several displays and notifications. From `pending.md`, candidates from the comparison.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **Follow-by-name and filter costs.** Attach by name instead of pid isn't offered; filters drop events after decoding, so they save storage and reading, not tracing cost; a followed program's output isn't captured; a followed program on macOS has no network; on Linux a program under a debugger can't be followed; Windows runs only a few kernel trace sessions at once (a ninth waits 8 s, then is refused). From `pending.md`, candidate 4.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [ ] 2026-11-28 (Mac, Linux, Windows): **A call stack with each event: remaining gaps.** Windows: network and memory events have no stack, a crash has no backtrace, no PDB shows offsets, stacks sent in two halves are not joined. macOS: crash backtrace is Apple silicon only, not for a program followed by `pid`, some system-library frames have no name, a program that changes user loses the handler; no report for a program that crashed repeatedly. From `pending.md`, candidate 5.
  - [ ] Mac
  - [ ] Linux
  - [ ] Windows

- [x] 2026-11-28 (Mac, Linux, Windows): **One set of trackers for every platform.** His request: SPECIFICATION.md as the spec, no pending.md; open port items moved into the trackers. In #52.
  - [x] Mac
  - [x] Linux
  - [x] Windows
