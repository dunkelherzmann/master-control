#!/usr/bin/env python3
"""A small GTK 4 entry dialog, like `zenity --entry`, for the Linux tree test to read and click.

    gtk_entry_dialog.py TITLE

A label "Your name", a field holding "start", and Cancel and OK buttons. OK prints the field's text and exits 0;
Cancel exits 1. Plain GTK 4 on purpose: zenity 4 is built on libadwaita, whose dialogs give assistive technology
positions that leave out their own padding, so a click by an element's id would miss (bugs.md). It closes after
two minutes if nothing is pressed.
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

title = sys.argv[1]
Gtk.init()
window = Gtk.Window(title=title, decorated=False)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin_top=16, margin_bottom=16, margin_start=16, margin_end=16)
window.set_child(box)
box.append(Gtk.Label(label="Your name", xalign=0))
entry = Gtk.Entry(text="start")
box.append(entry)
buttons = Gtk.Box(spacing=12, homogeneous=True)
cancel = Gtk.Button(label="Cancel")
ok = Gtk.Button(label="OK")
buttons.append(cancel)
buttons.append(ok)
box.append(buttons)

loop = GLib.MainLoop()
result = {"code": 1}


def answer(code):
    if code == 0:
        print(entry.get_text(), flush=True)
    result["code"] = code
    loop.quit()


ok.connect("clicked", lambda _button: answer(0))
cancel.connect("clicked", lambda _button: answer(1))
window.connect("close-request", lambda *_: answer(1))
window.present()
GLib.timeout_add_seconds(120, loop.quit)
loop.run()
sys.exit(result["code"])
