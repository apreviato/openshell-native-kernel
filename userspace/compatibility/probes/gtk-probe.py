#!/usr/bin/env python3
"""A GTK 3 window for the Linux apps smoke.

Appends JSON lines to $OPENSHELL_APP_PROBE_LOG, or else to
~/openshell-app-probe.log: where it runs, every size it takes, and the text
entered in its field when Enter is pressed, and the files it was given to
open. F2 opens a dialog with a field of its own, F3 copies CLIP to the
clipboard, and Escape closes the window.
"""
import json
import os
import sys
import time

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, Gtk  # noqa: E402

LOG = os.environ.get("OPENSHELL_APP_PROBE_LOG") or os.path.join(os.path.expanduser("~"), "openshell-app-probe.log")
CLIP = "openshell-app-clipboard"


def report(event, **fields):
    with open(LOG, "a", encoding="utf-8") as log:
        log.write(json.dumps({"app": "gtk", "event": event, "pid": os.getpid(), "at": time.time(), **fields}) + "\n")


def main():
    window = Gtk.Window(title="OpenShell GTK Probe")
    window.set_default_size(480, 320)
    entry = Gtk.Entry()
    entry.connect("activate", lambda field: report("text", value=field.get_text()))
    window.add(entry)
    sizes = []

    def configured(_window, event):
        size = (event.width, event.height)
        if not sizes or sizes[-1] != size:
            sizes.append(size)
            report("size", width=event.width, height=event.height)
        return False

    def open_dialog():
        dialog = Gtk.Dialog(title="OpenShell GTK Probe Dialog", transient_for=window, modal=True)
        field = Gtk.Entry()
        dialog.get_content_area().add(field)

        def answered(answer):
            report("dialog-text", value=answer.get_text())
            dialog.destroy()

        field.connect("activate", answered)
        dialog.show_all()
        field.grab_focus()
        report("dialog")

    def key(_window, event):
        if event.keyval == Gdk.KEY_Escape:
            window.destroy()
            return True
        if event.keyval == Gdk.KEY_F2:
            open_dialog()
            return True
        if event.keyval == Gdk.KEY_F3:
            clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
            clipboard.set_text(CLIP, -1)
            clipboard.store()
            report("copied", value=CLIP)
            return True
        return False

    window.connect("configure-event", configured)
    window.connect("key-press-event", key)
    window.connect("destroy", Gtk.main_quit)
    window.show_all()
    entry.grab_focus()
    report("start", display=os.environ.get("DISPLAY", ""), xauthority=os.environ.get("XAUTHORITY", ""), toolkit="gtk3", files=sys.argv[1:])
    Gtk.main()
    report("exit")


if __name__ == "__main__":
    main()
