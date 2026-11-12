#!/bin/sh
# Checks the current commit on the three machines: tests and clippy on the Mac, Linux and Windows. On Linux the
# screen and input tests use a private X server (Xvfb); on Windows they run in the signed-in desktop session.
# The work is local only (no GitHub yet), so the commit's files are sent to the other two over ssh.
#
#   scripts/check.sh            every machine
#   scripts/check.sh mac linux  only those
#   scripts/check.sh -w ...      send the working tree as it is, uncommitted changes included
#
# Without -w, Linux and Windows test the last commit. Prints one line per machine and exits
# non-zero if any failed. Logs go to .build/check/<machine>.log.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 2
# The other two machines, by the names ssh knows them under. They are yours to name: put them in scripts/hosts.env
# (not kept with the project), or in the environment. See scripts/hosts.env.example.
[ -f "$ROOT/scripts/hosts.env" ] && . "$ROOT/scripts/hosts.env"
LINUX_HOST="${MC_LINUX_HOST:-linux}"
WINDOWS_HOST="${MC_WINDOWS_HOST:-windows}"
LOGS="$ROOT/.build/check"
mkdir -p "$LOGS"
rm -f "$LOGS"/*.status
WORKTREE=0
if [ "${1:-}" = "-w" ]; then
    WORKTREE=1
    shift
fi
MACHINES="${*:-mac linux windows}"
# The files to test, as a tar stream: the last commit, or with -w every file git would track now.
send() {
    if [ "$WORKTREE" = 1 ]; then
        git ls-files -co --exclude-standard -z | COPYFILE_DISABLE=1 tar --no-xattrs -cf - --null -T -
    else
        git archive HEAD
    fi
}
if [ "$WORKTREE" = 0 ] && [ -n "$(git status --porcelain)" ]; then
    echo "check.sh: there are uncommitted changes; Linux and Windows test the last commit" >&2
fi
COMMIT="$(git rev-parse --short HEAD)"
# One folder per commit on the other machines, so two checks at once don't delete each other's files.
DIR="mc-check-$COMMIT"

for machine in $MACHINES; do
    case "$machine" in
    mac)
        (cargo test --workspace 2>&1 && cargo clippy --workspace --all-targets -- -D warnings 2>&1
         echo $? >"$LOGS/mac.status") >"$LOGS/mac.log" 2>&1 &
        ;;
    linux)
        (send | ssh -o ConnectTimeout=15 "$LINUX_HOST" "rm -rf /tmp/$DIR && mkdir -p /tmp/$DIR && tar -x -C /tmp/$DIR \
            && cd /tmp/$DIR && PATH=\$HOME/.cargo/bin:\$PATH CARGO_TARGET_DIR=\$HOME/.cache/mc-check-target timeout 1800 sh -c 'cargo test --workspace 2>&1 && cargo clippy --workspace --all-targets -- -D warnings 2>&1'; \
            code=\$?; rm -rf /tmp/$DIR; exit \$code" >"$LOGS/linux.log" 2>&1
         echo $? >"$LOGS/linux.status") &
        ;;
    windows)
        # Sent over ssh, but run through the desktop session (mctl, from the master-control skill): the screen and
        # input tests need a real desktop, which an ssh session doesn't have.
        (send | ssh -o ConnectTimeout=15 "$WINDOWS_HOST" "(if exist %TEMP%\\$DIR rmdir /s /q %TEMP%\\$DIR) & mkdir %TEMP%\\$DIR \
            && tar -x -C %TEMP%\\$DIR && cd /d %TEMP%\\$DIR && cd" >"$LOGS/windows.where" 2>&1 \
         && mctl exec windows --timeout 1800 --cwd "$(tail -1 "$LOGS/windows.where" | tr -d '\r')" -- \
            cmd /c "set CARGO_TARGET_DIR=%LOCALAPPDATA%\\mc-check-target&& cargo test --workspace 2>&1 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 && echo MC_CHECK_PASSED" \
            >"$LOGS/windows.log" 2>&1
         grep -q MC_CHECK_PASSED "$LOGS/windows.log"; echo $? >"$LOGS/windows.status"
         ssh -o ConnectTimeout=15 "$WINDOWS_HOST" "cd /d %TEMP% & rmdir /s /q %TEMP%\\$DIR" >/dev/null 2>&1) &
        ;;
    *)
        echo "check.sh: unknown machine $machine (mac, linux, windows)" >&2
        exit 2
        ;;
    esac
done
wait

failed=0
for machine in $MACHINES; do
    code="$(cat "$LOGS/$machine.status" 2>/dev/null || echo none)"
    passed="$(grep -E 'test result: ok' "$LOGS/$machine.log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ - | bc 2>/dev/null)"
    if [ "$code" = 0 ]; then
        echo "$machine: passed (${passed:-0} tests) at $COMMIT"
    else
        failed=1
        echo "$machine: FAILED (exit $code) at $COMMIT; see .build/check/$machine.log"
        grep -E 'panicked|FAILED|^error|failed' "$LOGS/$machine.log" | head -12
    fi
done
exit $failed
