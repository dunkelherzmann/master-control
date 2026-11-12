#!/bin/sh
# Builds the current commit and puts it everywhere Master Control is installed, then proves each copy is the new
# one by asking it for its version:
#   the Mac      the master (~/.local/opt/master-control/mctl) and /Applications/Master Control.app
#   Linux        the worker (service master-control) and the app on the desktop (the static build)
#   Windows      the worker (task "Master Control") and the app on the desktop
#
#   scripts/place.sh              all three
#   scripts/place.sh mac linux    only those
#
# The commit's files are sent over ssh (the work is local only), built there with the version worked out here, and
# the worker is replaced by running the new program's own `start`, which keeps its pairing. On Windows that step
# needs the desktop session, so it goes through the old skill's `mctl exec`. Stops at the first step that fails.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# The other two machines, by the names ssh knows them under. They are yours to name: put them in scripts/hosts.env
# (not kept with the project), or in the environment. See scripts/hosts.env.example.
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
echo "placing $VERSION ($COMMIT)"

# Fails unless the text given is exactly the version being placed.
same() { # what, what it said
    if [ "$2" != "$VERSION" ]; then
        echo "place.sh: $1 says \"$2\", not $VERSION" >&2
        exit 1
    fi
    echo "  $1: $2"
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
        git archive HEAD | ssh -o ConnectTimeout=15 "$LINUX_HOST" "set -e; rm -rf /tmp/$DIR; mkdir -p /tmp/$DIR; tar -x -C /tmp/$DIR; cd /tmp/$DIR
            export PATH=\$HOME/.cargo/bin:\$PATH CARGO_TARGET_DIR=\$HOME/.cache/mc-check-target MC_VERSION=$VERSION XDG_RUNTIME_DIR=/run/user/\$(id -u)
            cargo build --release -p mc-agent --bin mc-agent 2>&1 | tail -1
            cargo build --release -p master-control --target x86_64-unknown-linux-musl 2>&1 | tail -1
            \$CARGO_TARGET_DIR/release/mc-agent start --name linux >/dev/null
            cp \$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release/master-control ~/Desktop/master-control.new
            mv -f ~/Desktop/master-control.new ~/Desktop/master-control
            cd /; rm -rf /tmp/$DIR"
        same "Linux's app" "$(ssh -o ConnectTimeout=15 "$LINUX_HOST" '~/Desktop/master-control --version' | cut -d' ' -f2)"
        ;;
    windows)
        git archive HEAD | ssh -o ConnectTimeout=15 "$WINDOWS_HOST" "(if exist %TEMP%\\$DIR rmdir /s /q %TEMP%\\$DIR) & mkdir %TEMP%\\$DIR && tar -x -C %TEMP%\\$DIR"
        # Each program by name: with --bin for one, cargo builds only that one.
        ssh -o ConnectTimeout=15 "$WINDOWS_HOST" "cd /d %TEMP%\\$DIR && set CARGO_TARGET_DIR=%LOCALAPPDATA%\\mc-check-target&& set MC_VERSION=$VERSION&& cargo build --release -p mc-agent --bin mc-agent 2>&1 && cargo build --release -p master-control 2>&1" | tr -d '\r' | grep -E "Finished|^error" || true
        same "Windows' new worker program" "$(ssh -o ConnectTimeout=15 "$WINDOWS_HOST" '%LOCALAPPDATA%\mc-check-target\release\mc-agent.exe --version' | tr -d '\r' | cut -d' ' -f2)"
        mctl exec windows --timeout 120 -- cmd /c "%LOCALAPPDATA%\\mc-check-target\\release\\mc-agent.exe start --name windows --any-network 2>&1" | tail -1
        sleep 3
        same "Windows' app" "$(ssh -o ConnectTimeout=15 "$WINDOWS_HOST" 'copy /y "%LOCALAPPDATA%\mc-check-target\release\master-control.exe" "%USERPROFILE%\Desktop\Master Control.exe" >nul & "%USERPROFILE%\Desktop\Master Control.exe" --version & cd /d %TEMP% & rmdir /s /q %TEMP%\'"$DIR" | tr -d '\r' | head -1 | cut -d' ' -f2)"
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
