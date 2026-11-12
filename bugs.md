# Bugs

Every bug, in the order found. `- [ ]` open, `- [x]` fixed (with version and PR). Bugs found while testing are
marked *found in testing*.

- [x] 2026-11-11 — Windows — *found in testing*: `pull` on a folder answered "failed" with an access-denied message
  instead of saying the path is a folder. Fixed before the first build left the branch.
- [x] 2026-11-11 — all platforms — *found in testing*: the audit log kept the `text` of a `push`. Fixed before the
  first build left the branch.
- [x] 2026-11-11 — Linux — *found in testing*: `type_text` lost characters the keyboard layout has no key for (é) in
  GTK 4 apps, because the borrowed keycode was taken back before the app had read the key. Fixed before the build
  left the branch.
- [x] 2026-11-11 — Windows — *found in testing*: the observation test program split `PATH:TEXT` at the colon of `C:\`
  and wrote a file named `C` in the user's folder on the Windows machine. The file was removed and the separator
  changed before the build left the branch.
- [x] 2026-11-11 — Windows — *found in testing*: stopping an agent whose worker listens on every address (0.0.0.0) never
  returns on Windows; the test run hung for 14 minutes until stopped by hand. Tests now listen on the loopback
  address, which avoids it; a real worker still has it when stopped from inside (ending the process is unaffected).
  **Fixed 2026-11-11:** the listener now wakes its own waiting thread through the loopback address before it ends. Not run on
  Windows with an all-address listener: a test program doing that raises the firewall prompt.
- [x] 2026-11-11 — Windows — *found in testing*: the worker tests made Windows Firewall ask about each test program.
  Tests listen on the loopback address only now, so it no longer asks.
- [x] 2026-11-11 — Windows — *found in testing*: with a Windows Firewall prompt open, the worker can't bring any window
  forward (`focus_refused`) and clicks reach nothing; the prompt came from the app's own master Pair screen, which
  listens on the network. Seen while trying the app's window.
  **Fixed 2026-11-11:** the master's Start on Windows adds a firewall rule for the pairing port, so opening Pair raises
  no prompt, and Cleanup removes it; a refused focus now names the window that is in front instead. The rule's
  command lines are tested; it was not applied on the Windows machine, which is a worker.
- [x] 2026-11-11 — **`status` said the accessibility tree was missing everywhere** (all platforms): the
  `accessibility` capability was fixed at `false` although `tree`, `find` and `wait_for` work on Linux, Windows
  and macOS. It now says `true` where the tree can be read. Found while placing the Mac build.
- [x] 2026-11-11 — **Copying a new master program over the old one on the Mac got it killed** (macOS): `cp` onto
  the existing file keeps the old signature record and macOS kills the program at start (exit 137). The new
  file is now moved into place instead. Found while placing the Mac build.
- [x] 2026-11-11 — **The master couldn't observe a command on its own machine** (all platforms): the session's
  recorder is started by running the program again, and the master's program didn't know that command, so
  every `observe_start` with `machine: "master"` ended in "the tracer ended without finishing the session".
  The master's program and the app now run the recorder. Found when trying Mac observation through the
  installed master. From the command line (`mctl call`) each call is a new process, so a session there doesn't
  outlive its start; through the MCP server it does.
- [x] 2026-11-11 — **`toggle` on Windows sometimes answered with the old state** (Windows; found in testing, one
  run in four): the box was ticked, but its state was read back before the control had changed. The answer now
  waits for the change, up to a second; `expand` does the same.
- [x] 2026-11-11 — **A session on Windows failed at once when several were already running** (Windows; found in
  testing): Windows allows only a few trace sessions carrying kernel events at a time and answered "insufficient
  system resources". The recorder now waits up to 8 seconds for a place and then says what the limit is.
- [x] 2026-11-11 — **Edge accessibility lookup rejects an existing browser window** (macOS, installed
  `mctl` 0.1.12; *found in testing*): on this Mac, `windows` and `screenshot` found Edge window 1011,
  showing a YouTube video, but `find` with `name: "Pause"` or
  `name: "Play"` returned `not_supported: the app behind ... doesn't describe that window to assistive
  technology`. `status` reported `accessibility: true`. Through `exec`, System Events could read window 1's
  immediate UI elements (AXGroup and three AXButtons); this does not establish that the web player itself
  exposes an accessibility tree. Investigate window matching and distinguish an unavailable browser tree
  from a failure to locate the corresponding AX window. Enumerating the entire AX tree through an AppleScript
  fallback exceeded a 20-second command timeout.
  **Fixed 2026-11-11:** the cause was window matching by title: the window list calls the window "… - YouTube 🔊" while
  the accessibility layer words it differently, so no window matched. Windows are now matched by the window
  server's own number, then by title, then by place; and Chromium-based apps are told something is listening
  (`AXManualAccessibility`). Checked on the Claude and ChatGPT windows; Edge itself was on another desktop and
  could not be tried.
- [x] 2026-11-11 — **Window lookup and focus are unreliable around Edge popups and desktop transitions**
  (macOS, installed `mctl` 0.1.12; *found in testing*): after activating Edge and selecting its YouTube tab,
  `focus` reported window 1011 focused, but later `key` and `screenshot` returned `window_not_found` for
  that id; a subsequent `windows` listed the same id again. Other `key` and `click` calls returned
  `focus_refused` while the Edge window remained available to screenshots. `windows` sometimes marked
  an untitled 8×8 window or a 1335×199 address-bar popup focused instead of the browser window. Window
  coordinates also changed during desktop transitions. Possible popup/transition race; the cause and
  whether the focus refusals were appropriate still need reproduction under stable focus.
  **Fixed 2026-11-11:** the focused window is now the one the app itself names as having the keyboard, not simply its
  frontmost one, so a suggestions list or an 8×8 helper window no longer takes that place; a window missing
  from the list for a moment is looked for again (up to 0.6 s) before `window_not_found`. Not reproduced
  against Edge.
- [x] 2026-11-11 — **A successful key delivery can target Edge's address bar instead of the intended page**
  (macOS, installed `mctl` 0.1.12; *found in testing*): `key` with `window: 1011, keys: "k"` returned
  `delivered: true` while testing YouTube pause. The following screenshot showed `khttps://...` in the
  address field and its suggestions popup, rather than verified player control. No navigation was submitted;
  subsequent Escape events restored the displayed URL. Window focus alone does not establish content or
  player focus. Treat this as an element-focus/verification limitation: `delivered` is not proof that the
  intended page action happened. A reliable content-focus step and playback-state verification are needed.
  **Fixed 2026-11-11:** `type_text` and `key` take `element`: that element is given the keyboard first; and their answer
  carries `into`, the role and name of what had the keyboard (macOS, Windows), so "sent" can be told from
  "landed where meant". Verified on a real AppKit window and on Win32 controls, not on Edge.
- [x] 2026-11-11 — **Pointer movement failed while clicking the YouTube window** (macOS, installed
  `mctl` 0.1.12; *found in testing*): `click` with `window: 1011, x: 2500, y: 550` returned
  `failed: the pointer was sent to 2500,550 but is at 1585,814`. The preceding screenshot placed the
  window at x=1720, y=30, width=1720, height=1410, so the requested desktop point was inside it.
  `status` reported `input: true`. No successful click or pause was established. Investigate coordinate
  handling, desktop transitions and possible external pointer interference; the cause is unconfirmed.
  **Fixed 2026-11-11:** the pointer was read back the instant after it was sent, before the system had moved it; it is
  now given up to 150 ms to arrive and sent once more before a failure is called, and the failure says that
  something else is moving it. Whether that was the cause here isn't confirmed (a person was using the mouse).
- [x] 2026-11-11 — **The app said Claude Code wasn't installed, and never looked for Codex** (all platforms;
  reported by Herbert): Start looked for the `claude` command only on the search path, and an app opened from
  the desktop has a short one without `~/.local/bin`; Codex and the Claude app weren't looked for at all. Agents
  are now found by their settings folder, their app, or their command in the folders installers use.
- [x] 2026-11-11 — **A program installed from the app could be left unusable on a Mac** (macOS; found while
  looking into the above): Start copied the program over the old file, which on a Mac can get it killed as it
  starts, and the copy cut out of the app carried the app's signature, which doesn't fit a file on its own.
  The master and the worker are now written as a new file, signed on their own, and moved into place.
- [x] 2026-11-11 — **Installing the Mac worker again could fail with "Input/output error"** (macOS; found
  reading the code, not reproduced): the old launch agent was removed and the new one loaded at once, before
  launchd had let go of the old. The install now waits for that, and tries more than once. Its closing
  message also still said screen, input and observation weren't built for macOS.
- [ ] 2026-11-11 — **Once, a stopped Mac session didn't say how its target ended** (macOS; found in testing, one
  run in many): the verdict of a session stopped on request lacked `signal: SIGKILL`. The cause isn't known —
  most likely the event telling of the target's end didn't arrive. The session now also takes the answer from
  the system when it collects its own child, which covers that case; open until the cause is seen.
- [x] 2026-11-11 — **The Claude app lost its Master Control entry half an hour after Start** (macOS; found while
  drawing the status section): the app was open when Start wrote to its settings file; it keeps its settings in
  memory and wrote them back without the entry. Start now says so when the app is open — quit it, press Start
  again, then open it — instead of reporting it registered. Claude Code and Codex were not affected.
- [x] 2026-11-11 — **On macOS only a program's first crash has its backtrace** (macOS; found in testing): the
  stack comes from the system's crash report, and macOS writes none for a program that has crashed repeatedly.
  The crash problem says so (`no_stack_because`). Open: catch the crash ourselves instead of relying on the
  report.
  **Fixed 2026-11-11:** the session catches the crash itself as it happens and reads the stack then, so every
  crash of a program the session started has it, report or no report. A program followed by `pid` still relies
  on the report.
- [x] 2026-11-12 — **A Windows observation test fails now and then** (Windows; *found in testing*):
  `a_running_program_is_followed_and_left_running` answered "there is no running program with the process id …"
  once in three runs of the full check. The test's program idles four seconds before it acts, and when Windows has
  no trace session free the recorder waits up to eight for one, so the program can be gone before it is followed.
  The test should let its program idle longer than the recorder may wait. Seen again the same day: once in two
  runs of the check for #2.
  **Fixed 2026-11-12 (#5):** the program now waits for a file the test writes once the session has started,
  instead of idling a fixed time; the full check then passed three times in a row on Windows.
