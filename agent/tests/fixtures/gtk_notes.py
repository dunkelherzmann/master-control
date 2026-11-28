#!/usr/bin/env python3
"""A GTK 4 window with a text view that has the keyboard, above a one-line field, for the test that a click by id
on the field never types a tab into the text view on the way (Tab is a character there).

    gtk_notes.py TITLE STATE_FILE

It writes the text view's and the field's text to STATE_FILE (JSON) whenever either changes. It closes when its
input ends or after two minutes.
"""
import json
import os
import sys
import threading

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

title, state_file = sys.argv[1], sys.argv[2]
state = {"notes": "notes", "field": "field text"}


def note(**changes):
    state.update(changes)
    with open(state_file + ".new", "w") as out:
        json.dump(state, out)
    os.replace(state_file + ".new", state_file)


Gtk.init()
window = Gtk.Window(title=title)
window.set_default_size(360, 300)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin_top=12, margin_bottom=12, margin_start=12, margin_end=12)
window.set_child(box)
notes = Gtk.TextView(vexpand=True)
notes.get_buffer().set_text(state["notes"])
notes.get_buffer().connect("changed", lambda buffer: note(notes=buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)))
field = Gtk.Entry(text=state["field"])
field.connect("changed", lambda entry: note(field=entry.get_text()))
box.append(notes)
box.append(field)

loop = GLib.MainLoop()
window.connect("close-request", lambda *_: loop.quit())
window.present()
notes.grab_focus()
note()


def until_input_ends():
    sys.stdin.read()
    GLib.idle_add(loop.quit)


threading.Thread(target=until_input_ends, daemon=True).start()
GLib.timeout_add_seconds(120, loop.quit)
loop.run()
