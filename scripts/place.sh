#!/bin/sh
# Builds the current commit and puts it everywhere Master Control is installed, then proves each copy is the new
# one by asking it for its version:
#   the Mac      the master (~/.local/opt/master-control/mctl) and /Applications/Master Control.app
#   Linux        the app on the desktop: one static program that is also the worker (service master-control)
#   Windows      the worker (task "Master Control") and the app on the desktop
#
#   scripts/place.sh              all three
#   scripts/place.sh mac linux    only those
#
# The commit's files are sent through Master Control (mc-exec) and built there with the version worked out here. The
# worker is replaced last by the new program's own `start`, which keeps its pairing. That restarts the very worker
# mc-exec talks through, so `start` runs outside it (its own systemd unit on Linux, a scheduled task of its own on
# Windows) and the new worker is waited for and asked for its version. Stops at the first step that fails.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# The other two machines, by their Master Control names (`mctl machines`): linux and windows, or others put in
# scripts/hosts.env (not kept with the project) or the environment. See scripts/hosts.env.example.
[ -f "$ROOT/scripts/hosts.env" ] && . "$ROOT/scripts/hosts.env"
LINUX_HOST="${MC_LINUX_HOST:-linux}"
WINDOWS_HOST="${MC_WINDOWS_HOST:-windows}"
if [ -n "$(git status --porcelain)" ]; then
    echo "place.sh: there are uncommitted changes; commit first (the other machines get the last commit)" >&2
    exit 2
fi
VERSION="$(scripts/version.sh)"
COMMIT="$(git rev-parse --short HEAD)"
DIR="mc-place-$COMMIT"
MASTER="$HOME/.local/opt/master-control/mctl"
TAR="$(mktemp -t mc-place)"
trap 'rm -f "$TAR"' EXIT
echo "placing $VERSION ($COMMIT)"

# Fails unless the text given is exactly the version being placed.
same() { # what, what it said
    if [ "$2" != "$VERSION" ]; then
        echo "place.sh: $1 says \"$2\", not $VERSION" >&2
        exit 1
    fi
    echo "  $1: $2"
}

# Waits up to two minutes for the worker on that machine to answer with the version being placed.
restarted() { # machine
    for _ in $(seq 60); do
        [ "$("$MASTER" call "$1" status 2>/dev/null | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')" = "$VERSION" ] && return 0
        sleep 2
    done
    echo "place.sh: the worker on $1 didn't come back as $VERSION" >&2
    exit 1
}

for machine in ${*:-mac linux windows}; do
    case "$machine" in
    mac)
        cargo build --release -p mctl 2>&1 | tail -1
        # A new file moved into place: copying over the old one leaves a signature macOS then kills it for.
        cp target/release/mctl "$MASTER.new" && mv -f "$MASTER.new" "$MASTER"
        macos/scripts/build.sh >/dev/null
        rsync -a --delete "dist/Master Control.app/" "/Applications/Master Control.app/"
        codesign -v "/Applications/Master Control.app"
        same "the Mac's master" "$("$MASTER" --version | cut -d' ' -f2)"
        same "the Mac's app" "$("/Applications/Master Control.app/Contents/MacOS/master-control" --version | cut -d' ' -f2)"
        ;;
    linux)
        git archive HEAD >"$TAR"
        mc-exec "$LINUX_HOST" --put "$TAR" "/tmp/$DIR.tar"
        mc-exec "$LINUX_HOST" --timeout 1800 "set -e; rm -rf /tmp/$DIR; mkdir -p /tmp/$DIR; tar -x -C /tmp/$DIR -f /tmp/$DIR.tar; rm -f /tmp/$DIR.tar; cd /tmp/$DIR
            export PATH=\$HOME/.cargo/bin:\$PATH CARGO_TARGET_DIR=\$HOME/.cache/mc-check-target MC_VERSION=$VERSION
            cargo build --release -p master-control --target x86_64-unknown-linux-musl 2>&1 | tail -1
            cp \$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release/master-control ~/Desktop/master-control.new
            mv -f ~/Desktop/master-control.new ~/Desktop/master-control
            cd /; rm -rf /tmp/$DIR"
        same "Linux's app" "$(mc-exec "$LINUX_HOST" '~/Desktop/master-control --version' | cut -d' ' -f2)"
        # A unit of its own, so restarting the worker's service doesn't stop it halfway.
        mc-exec "$LINUX_HOST" "systemd-run --user --collect --quiet --unit=mc-place-$COMMIT ~/Desktop/master-control agent start --name linux"
        restarted "$LINUX_HOST"
        ;;
    windows)
        WTEMP="$(mc-exec "$WINDOWS_HOST" "echo %TEMP%" | tr -d '\r\n')"
        [ -n "$WTEMP" ] || { echo "place.sh: $WINDOWS_HOST didn't answer" >&2; exit 1; }
        git archive HEAD >"$TAR"
        mc-exec "$WINDOWS_HOST" --put "$TAR" "$WTEMP\\$DIR.tar"
        mc-exec "$WINDOWS_HOST" "(if exist %TEMP%\\$DIR rmdir /s /q %TEMP%\\$DIR) & mkdir %TEMP%\\$DIR && tar -x -C %TEMP%\\$DIR -f %TEMP%\\$DIR.tar && del /q %TEMP%\\$DIR.tar"
        # Each program by name: with --bin for one, cargo builds only that one.
        mc-exec "$WINDOWS_HOST" --timeout 1800 "cd /d %TEMP%\\$DIR && set CARGO_TARGET_DIR=%LOCALAPPDATA%\\mc-check-target&& set MC_VERSION=$VERSION&& cargo build --release -p mc-agent --bin mc-agent 2>&1 && cargo build --release -p master-control 2>&1" | tr -d '\r' | grep -E "Finished|^error" || true
        same "Windows' new worker program" "$(mc-exec "$WINDOWS_HOST" '%LOCALAPPDATA%\mc-check-target\release\mc-agent.exe --version' | tr -d '\r' | cut -d' ' -f2)"
        same "Windows' app" "$(mc-exec "$WINDOWS_HOST" 'copy /y "%LOCALAPPDATA%\mc-check-target\release\master-control.exe" "%USERPROFILE%\Desktop\Master Control.exe" >nul & "%USERPROFILE%\Desktop\Master Control.exe" --version & cd /d %TEMP% & rmdir /s /q %TEMP%\'"$DIR" | tr -d '\r' | head -1 | cut -d' ' -f2)"
        # A scheduled task of its own (in the desktop session, with admin rights), so ending the worker's task
        # doesn't end it halfway. It and its log are cleared once the new worker answers.
        printf '"%%LOCALAPPDATA%%\\mc-check-target\\release\\mc-agent.exe" start --name windows --any-network >"%%TEMP%%\\%s-start.log" 2>&1\r\n' "$DIR" >"$TAR"
        mc-exec "$WINDOWS_HOST" --put "$TAR" "$WTEMP\\$DIR-start.cmd"
        mc-exec "$WINDOWS_HOST" "schtasks /Create /TN \"$DIR\" /TR \"%TEMP%\\$DIR-start.cmd\" /SC ONCE /ST 00:00 /RL HIGHEST /F >nul 2>&1 && schtasks /Run /TN \"$DIR\" >nul"
        restarted "$WINDOWS_HOST"
        mc-exec "$WINDOWS_HOST" "schtasks /Delete /TN \"$DIR\" /F >nul & type %TEMP%\\$DIR-start.log & del /q %TEMP%\\$DIR-start.log %TEMP%\\$DIR-start.cmd" | tr -d '\r' | tail -1
        ;;
    *)
        echo "place.sh: no machine called $machine (mac, linux, windows)" >&2
        exit 2
        ;;
    esac
done

# The running workers and the master, asked through the master itself.
for machine in ${*:-mac linux windows}; do
    name="$machine"
    [ "$machine" = mac ] && name=master
    same "$name, running" "$("$MASTER" call "$name" status | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')"
done
echo "placed $VERSION on: ${*:-mac linux windows}"
