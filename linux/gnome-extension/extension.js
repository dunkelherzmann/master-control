// Master Control's helper inside GNOME Shell on Wayland. Wayland gives other programs no window list, no picture of
// the screen and no way to send input; the shell has all three. This serves them on the session bus to the agent,
// which runs as the same user. Answers that hold several values are JSON.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

Gio._promisify(Shell.Screenshot.prototype, 'screenshot_area');

const NAME = 'io.github.dunkelherzmann.MasterControl';
const PATH = '/io/github/dunkelherzmann/MasterControl';
// Raised whenever a method changes, so the agent can tell an old copy still loaded in the shell.
const API = 4;

const INTERFACE = `
<node>
  <interface name="${NAME}">
    <method name="Api"><arg type="u" direction="out" name="version"/></method>
    <method name="Screen"><arg type="s" direction="out" name="json"/></method>
    <method name="Windows"><arg type="s" direction="out" name="json"/></method>
    <method name="Activate"><arg type="t" direction="in" name="id"/><arg type="b" direction="out" name="done"/></method>
    <method name="Place">
      <arg type="t" direction="in" name="id"/><arg type="i" direction="in" name="x"/><arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/><arg type="i" direction="in" name="height"/><arg type="b" direction="out" name="done"/>
    </method>
    <method name="SetState"><arg type="t" direction="in" name="id"/><arg type="s" direction="in" name="state"/><arg type="b" direction="out" name="done"/></method>
    <method name="Workspaces"><arg type="s" direction="out" name="json"/></method>
    <method name="Workspace"><arg type="s" direction="in" name="action"/><arg type="i" direction="in" name="index"/><arg type="b" direction="out" name="done"/></method>
    <method name="Capture">
      <arg type="i" direction="in" name="x"/><arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/><arg type="i" direction="in" name="height"/>
      <arg type="ay" direction="out" name="png"/>
    </method>
    <method name="Pointer"><arg type="i" direction="out" name="x"/><arg type="i" direction="out" name="y"/></method>
    <method name="MovePointer"><arg type="d" direction="in" name="x"/><arg type="d" direction="in" name="y"/></method>
    <method name="Button"><arg type="u" direction="in" name="button"/><arg type="b" direction="in" name="down"/></method>
    <method name="Scroll"><arg type="i" direction="in" name="right"/><arg type="i" direction="in" name="down"/></method>
    <method name="Key"><arg type="u" direction="in" name="keyval"/><arg type="b" direction="in" name="down"/></method>
    <method name="Text"><arg type="s" direction="in" name="text"/><arg type="s" direction="out" name="how"/></method>
    <method name="GetClipboard"><arg type="s" direction="out" name="text"/></method>
    <method name="SetClipboard"><arg type="s" direction="in" name="text"/></method>
    <method name="IdleTime"><arg type="t" direction="out" name="ms"/></method>
    <method name="Ask"><arg type="s" direction="in" name="text"/><arg type="u" direction="out" name="id"/></method>
    <method name="Answer"><arg type="u" direction="in" name="id"/><arg type="s" direction="out" name="answer"/></method>
    <method name="Tell"><arg type="s" direction="in" name="text"/></method>
  </interface>
</node>`;

// The kinds of window a person works in; docks, the desktop and menus are left out.
const LISTED = new Set([
    Meta.WindowType.NORMAL, Meta.WindowType.DIALOG, Meta.WindowType.MODAL_DIALOG, Meta.WindowType.UTILITY,
    Meta.WindowType.TOOLBAR, Meta.WindowType.SPLASHSCREEN,
]);

// The extension stays on while the screen is locked (session mode unlock-dialog), so the agent can say the screen
// is locked instead of losing the shell; it refuses pictures and input until the session is unlocked.
const LOCKED = 'the screen is locked: unlock the session first (as root: loginctl unlock-session)';

function unlocked() {
    if (Main.sessionMode.isLocked)
        throw new Error(LOCKED);
}

function windowById(id) {
    return global.get_window_actors().map(actor => actor.meta_window).find(window => window.get_id().toString() === id.toString());
}

function now() {
    return GLib.get_monotonic_time();
}

class Service {
    constructor() {
        // GNOME 51 dropped Clutter.get_default_backend(); the stage's context has the backend since 46.
        const backend = global.stage.context?.get_backend?.() ?? Clutter.get_default_backend();
        const seat = backend.get_default_seat();
        this._pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._held = new Set();
        // The pop-ups asked, by number: '' while open, then 'ok' or 'cancel'.
        this._answers = new Map();
        this._dialogs = new Set();
        this._told = new Set();
        this._asked = 0;
        this._exported = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._exported.export(Gio.DBus.session, PATH);
        this._owner = Gio.bus_own_name_on_connection(Gio.DBus.session, NAME, Gio.BusNameOwnerFlags.REPLACE, null, null);
    }

    destroy() {
        // Nothing stays pressed once the helper is gone.
        for (const keyval of this._held)
            this._keyboard.notify_keyval(now(), keyval, Clutter.KeyState.RELEASED);
        for (const dialog of this._dialogs)
            dialog.close();
        Gio.bus_unown_name(this._owner);
        this._exported.unexport();
        this._pointer.run_dispose();
        this._keyboard.run_dispose();
    }

    Api() {
        return API;
    }

    Screen() {
        const [width, height] = global.display.get_size();
        const monitors = [];
        for (let index = 0; index < global.display.get_n_monitors(); index++) {
            const rect = global.display.get_monitor_geometry(index);
            monitors.push({x: rect.x, y: rect.y, width: rect.width, height: rect.height, scale: global.display.get_monitor_scale(index)});
        }
        return JSON.stringify({width, height, monitors, locked: Main.sessionMode.isLocked});
    }

    Windows() {
        const focused = global.display.focus_window;
        // Bottom first; the agent wants the front first.
        const windows = global.get_window_actors().map(actor => actor.meta_window).reverse();
        return JSON.stringify(windows
            .filter(window => LISTED.has(window.get_window_type()) && !window.is_override_redirect())
            .map(window => {
                const frame = window.get_frame_rect();
                // Where the app counts its positions from: below the title bar the shell draws for it, if it draws
                // one; else the visible frame (the client rectangle of an app that draws its own frame includes its
                // invisible shadow border).
                const client = window.decorated ? window.frame_rect_to_client_rect(frame) : frame;
                return {
                    id: window.get_id().toString(),
                    title: window.get_title() ?? '',
                    app: window.get_wm_class() || window.get_gtk_application_id() || window.get_sandboxed_app_id() || '',
                    pid: window.get_pid(),
                    x: frame.x, y: frame.y, width: frame.width, height: frame.height,
                    client_x: client.x, client_y: client.y,
                    focused: window === focused,
                    minimized: window.minimized,
                };
            }));
    }

    Activate(id) {
        unlocked();
        const window = global.get_window_actors().map(actor => actor.meta_window).find(window => window.get_id().toString() === id.toString());
        if (!window)
            return false;
        Main.activateWindow(window);
        return true;
    }

    // The frame as Windows() gives it. A maximized or minimized window is restored first.
    Place(id, x, y, width, height) {
        unlocked();
        const window = windowById(id);
        if (!window)
            return false;
        if (window.minimized)
            window.unminimize();
        if (window.is_maximized())
            window.unmaximize();
        window.move_resize_frame(true, x, y, width, height);
        return true;
    }

    SetState(id, state) {
        unlocked();
        const window = windowById(id);
        if (!window)
            return false;
        if (state === 'minimized') {
            window.minimize();
        } else {
            if (window.minimized)
                window.unminimize();
            if (state === 'maximized')
                window.maximize();
            else if (window.is_maximized())
                window.unmaximize();
        }
        return true;
    }

    Workspaces() {
        const manager = global.workspace_manager;
        const names = [];
        for (let index = 0; index < manager.get_n_workspaces(); index++)
            names.push(Meta.prefs_get_workspace_name?.(index) ?? '');
        return JSON.stringify({active: manager.get_active_workspace_index(), names, dynamic: Meta.prefs_get_dynamic_workspaces?.() ?? false});
    }

    // switch, create (a new one at the end) or remove, by index from 0.
    Workspace(action, index) {
        unlocked();
        const manager = global.workspace_manager;
        const time = global.get_current_time();
        if (action === 'create') {
            manager.append_new_workspace(false, time);
            return true;
        }
        const workspace = manager.get_workspace_by_index(index);
        if (!workspace)
            return false;
        if (action === 'switch')
            workspace.activate(time);
        else if (action === 'remove')
            manager.remove_workspace(workspace, time);
        else
            return false;
        return true;
    }

    async CaptureAsync([x, y, width, height], invocation) {
        try {
            unlocked();
            const stream = Gio.MemoryOutputStream.new_resizable();
            await new Shell.Screenshot().screenshot_area(x, y, width, height, stream);
            stream.close(null);
            invocation.return_value(new GLib.Variant('(ay)', [stream.steal_as_bytes().get_data()]));
        } catch (error) {
            invocation.return_error_literal(Gio.DBusError, Gio.DBusError.FAILED, error.message === LOCKED ? LOCKED : `the picture failed: ${error.message}`);
        }
    }

    Pointer() {
        const [x, y] = global.get_pointer();
        return [x, y];
    }

    MovePointer(x, y) {
        unlocked();
        this._pointer.notify_absolute_motion(now(), x, y);
    }

    Button(button, down) {
        if (down)
            unlocked();
        this._pointer.notify_button(now(), button, down ? Clutter.ButtonState.PRESSED : Clutter.ButtonState.RELEASED);
    }

    Scroll(right, down) {
        unlocked();
        const turn = (count, forward, back) => {
            for (let notch = 0; notch < Math.abs(count); notch++)
                this._pointer.notify_discrete_scroll(now(), count > 0 ? forward : back, Clutter.ScrollSource.WHEEL);
        };
        turn(down, Clutter.ScrollDirection.DOWN, Clutter.ScrollDirection.UP);
        turn(right, Clutter.ScrollDirection.RIGHT, Clutter.ScrollDirection.LEFT);
    }

    Key(keyval, down) {
        if (down) {
            unlocked();
            this._held.add(keyval);
        }
        else
            this._held.delete(keyval);
        this._keyboard.notify_keyval(now(), keyval, down ? Clutter.KeyState.PRESSED : Clutter.KeyState.RELEASED);
    }

    // Plain ASCII goes in as key presses, the way the on-screen keyboard sends it. Anything else needs the input
    // method: key presses only reach characters the keyboard layout has, which leaves out most of Unicode.
    Text(text) {
        unlocked();
        const ascii = /^[\x20-\x7e\n\t]*$/.test(text);
        if (!ascii && Main.inputMethod.currentFocus) {
            Main.inputMethod.commit(text);
            return 'input method';
        }
        for (const character of text) {
            const keyval = character === '\n' ? Clutter.KEY_Return
                : character === '\t' ? Clutter.KEY_Tab
                    : Clutter.unicode_to_keysym(character.codePointAt(0));
            this._keyboard.notify_keyval(now(), keyval, Clutter.KeyState.PRESSED);
            this._keyboard.notify_keyval(now(), keyval, Clutter.KeyState.RELEASED);
        }
        return ascii ? 'keys' : 'keys (no text field with an input method has the keyboard; characters the layout lacks are lost)';
    }

    async GetClipboardAsync(_parameters, invocation) {
        if (Main.sessionMode.isLocked) {
            invocation.return_error_literal(Gio.DBusError, Gio.DBusError.FAILED, LOCKED);
            return;
        }
        const text = await new Promise(resolve => St.Clipboard.get_default().get_text(St.ClipboardType.CLIPBOARD, (_clipboard, got) => resolve(got)));
        invocation.return_value(new GLib.Variant('(s)', [text ?? '']));
    }

    SetClipboard(text) {
        unlocked();
        St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text);
    }

    // How long since the last input, as the shell's idle monitor counts it.
    IdleTime() {
        const monitor = global.backend?.get_core_idle_monitor?.() ?? Meta.IdleMonitor?.get_core?.();
        if (!monitor)
            throw new Error('the shell has no idle monitor');
        return monitor.get_idletime();
    }

    // Opens a pop-up with Cancel and OK over everything, and answers at once with its number; Answer tells when it
    // has been answered. No button is the default, so a person still typing can't answer by accident; Escape is
    // Cancel. A D-Bus call left waiting for a person could be cut off by the bus.
    Ask(text) {
        unlocked();
        // A "done" pop-up still open would sit where this one opens, and a click meant for it would answer this.
        for (const told of this._told)
            told.close(global.get_current_time());
        this._told.clear();
        const id = ++this._asked;
        const dialog = new ModalDialog.ModalDialog({styleClass: 'prompt-dialog', destroyOnClose: true});
        dialog.contentLayout.add_child(new Dialog.MessageDialogContent({title: 'Master Control', description: text}));
        const finish = answer => {
            this._answers.set(id, answer);
            this._dialogs.delete(dialog);
            dialog.close(global.get_current_time());
        };
        dialog.setButtons([
            {label: 'Cancel', action: () => finish('cancel'), key: Clutter.KEY_Escape},
            {label: 'OK', action: () => finish('ok')},
        ]);
        this._answers.set(id, '');
        this._dialogs.add(dialog);
        if (!dialog.open(global.get_current_time())) {
            this._answers.delete(id);
            this._dialogs.delete(dialog);
            dialog.destroy();
            throw new Error('the pop-up couldn\'t be shown (another one may hold the screen)');
        }
        return id;
    }

    // '' while the pop-up is open, then its answer (asked once more after that: 'gone').
    Answer(id) {
        const answer = this._answers.get(id);
        if (answer === undefined)
            return 'gone';
        if (answer !== '')
            this._answers.delete(id);
        return answer;
    }

    // A pop-up with one OK button, answered at once: nobody waits for it to be clicked.
    Tell(text) {
        // Never over a question still waiting for its answer: it would hide it.
        if ([...this._answers.values()].includes(''))
            return;
        for (const told of this._told)
            told.close(global.get_current_time());
        this._told.clear();
        const dialog = new ModalDialog.ModalDialog({styleClass: 'prompt-dialog', destroyOnClose: true});
        dialog.contentLayout.add_child(new Dialog.MessageDialogContent({title: 'Master Control', description: text}));
        dialog.setButtons([{
            label: 'OK',
            action: () => {
                this._dialogs.delete(dialog);
                this._told.delete(dialog);
                dialog.close(global.get_current_time());
            },
            key: Clutter.KEY_Escape,
            default: true,
        }]);
        this._dialogs.add(dialog);
        this._told.add(dialog);
        if (!dialog.open(global.get_current_time())) {
            this._dialogs.delete(dialog);
            this._told.delete(dialog);
            dialog.destroy();
        }
    }
}

export default class MasterControlExtension extends Extension {
    enable() {
        this._service = new Service();
    }

    disable() {
        this._service?.destroy();
        this._service = null;
    }
}
