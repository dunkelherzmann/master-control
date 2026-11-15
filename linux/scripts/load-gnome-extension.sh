#!/bin/sh
# Loads the installed Master Control GNOME Shell extension into the running shell without signing out: for
# trying a change, or for a first install on a machine nobody will sign in to soon. Run on the Linux machine, in
# the user's unlocked GNOME session (Wayland), after `mc-agent start` has put the files in place.
#
# GNOME Shell reads new extensions only at sign-in, and GJS caches modules by path, so a copy goes into a new
# folder (named after the UUID, which the shell requires) and is loaded from there through Looking Glass.
# Looking Glass is opened and typed into with Mutter's RemoteDesktop D-Bus API, which asks for no permission.
# At the next sign-in the shell loads the installed copy as usual.
set -eu
UUID=master-control@dunkelherzmann.github.io
: "${DBUS_SESSION_BUS_ADDRESS:=unix:path=/run/user/$(id -u)/bus}"
export DBUS_SESSION_BUS_ADDRESS
SOURCE="$HOME/.local/share/gnome-shell/extensions/$UUID"
[ -f "$SOURCE/extension.js" ] || { echo "the extension isn't installed: run mc-agent start first" >&2; exit 1; }
LOCKED=$(gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver --method org.gnome.ScreenSaver.GetActive)
[ "$LOCKED" = "(false,)" ] || { echo "the screen is locked: unlock it first (as root: loginctl unlock-session)" >&2; exit 1; }
COPY="$(mktemp -d /tmp/mc-ext-XXXXXX)/$UUID"
mkdir -p "$COPY" && cp "$SOURCE"/* "$COPY"/
OUT="$(dirname "$COPY")/result"
CODE="let out=''; try { const m=Main.extensionManager; const old=m.lookup('$UUID'); if (old) await m.unloadExtension(old); const e=m.createExtensionObject('$UUID', Gio.File.new_for_path('$COPY'), 2); await m.loadExtension(e); out='state '+e.state+' '+(e.error||''); } catch (x) { out='failed: '+x; } GLib.file_set_contents('$OUT', out); out"
python3 - "$CODE" <<'PY'
import sys, time
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
def call(path, interface, method, args=None, reply=None):
    return bus.call_sync("org.gnome.Mutter.RemoteDesktop", path, interface, method, args,
                         GLib.VariantType(reply) if reply else None, 0, -1, None)
session = call("/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop", "CreateSession", None, "(o)")[0]
S = "org.gnome.Mutter.RemoteDesktop.Session"
call(session, S, "Start")
def key(symbol, down):
    call(session, S, "NotifyKeyboardKeysym", GLib.Variant("(ub)", (symbol, down)))
def tap(symbol):
    key(symbol, True); key(symbol, False); time.sleep(0.05)
def text(line):
    for ch in line:
        tap(ord(ch) if ord(ch) < 0x100 else 0x01000000 | ord(ch))
ALT, F2, RETURN, ESCAPE = 0xffe9, 0xffbf, 0xff0d, 0xff1b
tap(ESCAPE); time.sleep(0.5)          # nothing half-open (a dialog, Looking Glass) takes the keys
key(ALT, True); tap(F2); key(ALT, False); time.sleep(1)
text("lg"); tap(RETURN); time.sleep(2)
text(sys.argv[1]); tap(RETURN); time.sleep(2)
tap(ESCAPE)
call(session, S, "Stop")
PY
cat "$OUT" 2>/dev/null || { echo "Looking Glass didn't run the load (was another window in the way?)" >&2; exit 1; }
echo
gdbus call --session --dest io.github.dunkelherzmann.MasterControl --object-path /io/github/dunkelherzmann/MasterControl --method io.github.dunkelherzmann.MasterControl.Api
