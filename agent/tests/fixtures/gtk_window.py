#!/usr/bin/env python3
"""A small GTK 4 window with ordinary controls, for the Linux element-action tests to act on.

    gtk_window.py TITLE STATE_FILE

Whenever something in it changes it rewrites STATE_FILE (JSON), so a test can see what really happened to the
controls, apart from what the accessibility tree says. It closes when its input ends or after two minutes.
"""
import json
import sys
import threading

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

title, state_file = sys.argv[1], sys.argv[2]
state = {"presses": 0, "text": "first text", "ticked": True, "switched": False, "expanded": False, "selected": None}


def note(**changes):
    state.update(changes)
    with open(state_file + ".new", "w") as out:
        json.dump(state, out)
    import os

    os.replace(state_file + ".new", state_file)


Gtk.init()
window = Gtk.Window(title=title)
window.set_default_size(360, 420)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin_top=12, margin_bottom=12, margin_start=12, margin_end=12)
window.set_child(box)

label = Gtk.Label(label="A label")
button = Gtk.Button(label="Save it")


def pressed(_button):
    note(presses=state["presses"] + 1)
    label.set_label(f"pressed {state['presses']}")


button.connect("clicked", pressed)
entry = Gtk.Entry(text="first text")
entry.connect("changed", lambda field: note(text=field.get_text()))
check = Gtk.CheckButton(label="Enable thing", active=True)
check.connect("toggled", lambda box_: note(ticked=box_.get_active()))
switch = Gtk.Switch(halign=Gtk.Align.START)
switch.connect("notify::active", lambda widget, _: note(switched=widget.get_active()))
expander = Gtk.Expander(label="More")
expander.set_child(Gtk.Label(label="inside"))
expander.connect("notify::expanded", lambda widget, _: note(expanded=widget.get_expanded()))
rows = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
for name in ("One", "Two", "Three"):
    rows.append(Gtk.Label(label=name))
rows.connect("row-selected", lambda _list, row: note(selected=row.get_child().get_label() if row else None))
for widget in (button, entry, label, check, switch, expander, rows):
    box.append(widget)

loop = GLib.MainLoop()
window.connect("close-request", lambda *_: loop.quit())
window.present()
note()


def until_input_ends():
    sys.stdin.read()
    GLib.idle_add(loop.quit)


threading.Thread(target=until_input_ends, daemon=True).start()
GLib.timeout_add_seconds(120, loop.quit)
loop.run()
