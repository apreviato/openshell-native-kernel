#!/usr/bin/env python3
"""A GTK 3 window for the Linux apps smoke.

Appends JSON lines to $OPENSHELL_APP_PROBE_LOG, or else to
~/openshell-app-probe.log: where it runs, every size it takes, and the text
entered in its field when Enter is pressed. Escape closes it.
"""
import json
import os
import time

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, Gtk  # noqa: E402

LOG = os.environ.get("OPENSHELL_APP_PROBE_LOG") or os.path.join(os.path.expanduser("~"), "openshell-app-probe.log")


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

    def key(_window, event):
        if event.keyval == Gdk.KEY_Escape:
            window.destroy()
            return True
        return False

    window.connect("configure-event", configured)
    window.connect("key-press-event", key)
    window.connect("destroy", Gtk.main_quit)
    window.show_all()
    entry.grab_focus()
    report("start", display=os.environ.get("DISPLAY", ""), xauthority=os.environ.get("XAUTHORITY", ""), toolkit="gtk3")
    Gtk.main()
    report("exit")


if __name__ == "__main__":
    main()
