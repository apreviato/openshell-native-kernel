#!/usr/bin/python3
"""The live medium's setup screen: what its boot menu's "Install OpenShell
Native on this computer" and "Update installed OpenShell Native (keep
files)" open instead of the Desktop, as OpenShell Live's do (its installer
and its updater, update_gui.py, whose look this is).

    openshell_setup.py install    the installer's steps, as Calamares asks
                                  them on Live: time zone, keyboard, disk,
                                  account, summary, then the copy
    openshell_setup.py update     the installations on this computer, the
                                  update (or the previous release back)

It runs as the session's user under the session supervisor, which does the
work as root (install.rs, usb_update.rs) and powers the computer off or
restarts it (power.rs); setup_model.py speaks to it. Leaving it with "Try
OpenShell Native" starts the live Desktop instead (installed-session.sh)."""
import os
import subprocess
import sys
import threading

import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, GLib, Gtk, Pango

# Isolated (python3 -I): its own folder is not on the path by itself.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import setup_model as model  # noqa: E402

# OpenShell Live's updater (live/config/includes.chroot/usr/local/lib/openshell/update_gui.py).
CSS = b"""
window { background: #080f18; color: #e7edf6; font-family: 'Noto Sans', sans-serif; font-size: 14px; }
label { color: #e7edf6; }
.brand { color: #8dadff; font-size: 15px; font-weight: bold; letter-spacing: 2px; }
.title { font-size: 30px; font-weight: bold; }
.muted { color: #a2b1c5; }
.small { font-size: 12px; }
.card { background: #121e2d; border: 1px solid #25354a; border-radius: 14px; padding: 20px; }
.card-title { font-size: 18px; font-weight: bold; }
.notice { background: #20283a; border: 1px solid #415678; border-radius: 10px; padding: 16px; }
.error { color: #ffb2ba; }
button { background: #182638; color: #e7edf6; border: 1px solid #334760; border-radius: 9px; padding: 11px 20px; box-shadow: none; text-shadow: none; }
button:hover { background: #22364e; border-color: #799ce3; }
button:focus { outline-color: #9bb7ff; outline-width: 2px; outline-style: solid; outline-offset: 2px; }
button:disabled { opacity: 0.45; }
button.primary { background: #88aaff; border-color: #88aaff; color: #0b172a; font-weight: bold; }
button.primary label { color: #0b172a; }
button.primary:hover { background: #a4bfff; }
button.quiet { background: transparent; }
list { background: transparent; }
row { padding: 16px; margin-bottom: 8px; border-radius: 10px; background: #121e2d; border: 1px solid #2a3a50; }
row:selected { background: #203453; border-color: #88aaff; }
row:focus { outline-color: #9bb7ff; }
entry, combobox button { background: #0c1623; color: #e7edf6; border: 1px solid #3a506c; border-radius: 8px; padding: 9px 12px; }
entry:focus { border-color: #9bb7ff; }
switch { background: #33445d; border-radius: 14px; }
switch:checked { background: #88aaff; }
switch slider { background: #e7edf6; border-radius: 12px; }
progressbar trough { background: #223149; min-height: 8px; border-radius: 6px; }
progressbar progress { background: #88aaff; min-height: 8px; border-radius: 6px; }
.step { padding: 6px 0; color: #8494ac; }
.step.active { color: #a9c1ff; font-weight: bold; }
.step.done { color: #91d5b6; }
"""

INSTALL_STEPS = [("location", "Location"), ("keyboard", "Keyboard"), ("disk", "Disk"), ("account", "Account"), ("summary", "Summary")]
# The status the supervisor's jobs are polled at, in milliseconds.
POLL_MS = 500


def styled(widget, *classes):
    for name in classes:
        widget.get_style_context().add_class(name)
    return widget


def label(text, *classes):
    widget = Gtk.Label(label=str(text), xalign=0)
    widget.set_line_wrap(True)
    widget.set_line_wrap_mode(Pango.WrapMode.WORD_CHAR)
    widget.set_max_width_chars(85)
    return styled(widget, *classes)


class SetupWindow(Gtk.Window):
    def __init__(self, mode, supervisor=None, fullscreen=True):
        super().__init__(title="OpenShell Native · " + ("Install" if mode == "install" else "Update from USB"))
        self.mode = mode
        self.supervisor = supervisor or model.Supervisor()
        self.busy = False
        self.page = None
        self.poll_source = None
        # The installer's answers, and the updater's choice.
        self.zone = model.current_zone() or "UTC"
        self.layout, self.variant = self.current_keyboard()
        self.disk = None
        self.target = None
        self.set_default_size(1060, 760)
        self.set_decorated(False)
        self.connect("delete-event", lambda *_: True)
        provider = Gtk.CssProvider()
        provider.load_from_data(CSS)
        Gtk.StyleContext.add_provider_for_screen(Gdk.Screen.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

        shell = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20)
        shell.set_border_width(32)
        self.add(shell)
        header = Gtk.Box(spacing=20)
        brand = label("OPENSHELL", "brand")
        brand.set_line_wrap(False)
        subtitle = label("INSTALL" if mode == "install" else "UPDATE FROM USB", "small", "muted")
        subtitle.set_line_wrap(False)
        header.pack_start(brand, False, False, 0)
        header.pack_end(subtitle, False, False, 0)
        shell.pack_start(header, False, False, 0)
        self.steps_panel = Gtk.Box(spacing=18)
        shell.pack_start(self.steps_panel, False, False, 0)

        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        self.body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=18)
        self.body.set_valign(Gtk.Align.CENTER)
        scroll.add(self.body)
        shell.pack_start(scroll, True, True, 0)
        self.notice_panel = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        shell.pack_start(self.notice_panel, False, False, 0)
        self.actions_panel = Gtk.Box(spacing=10)
        shell.pack_start(self.actions_panel, False, False, 0)
        footer = Gtk.Box(spacing=16)
        hint = "Nothing changes on this computer until you confirm." if mode == "install" else \
            "Your files, accounts, settings and installed apps are kept."
        footer.pack_start(label(hint, "small", "muted"), True, True, 0)
        self.shutdown_button = self.button("Shut down", self.request_shutdown, "quiet")
        footer.pack_end(self.shutdown_button, False, False, 0)
        self.live_button = self.button("Try OpenShell Native", self.try_live, "quiet")
        footer.pack_end(self.live_button, False, False, 0)
        shell.pack_end(footer, False, False, 0)
        if fullscreen:
            self.fullscreen()
        self.show_all()
        if mode == "install":
            self.show_welcome()
        else:
            self.prepare_update()

    # ── Pieces ───────────────────────────────────────────────────────────

    def button(self, text, callback, *classes):
        widget = styled(Gtk.Button(label=text), *classes)
        widget.connect("clicked", lambda *_: callback())
        return widget

    def clear(self, page, title, description=None, step=None):
        self.page = page
        for panel in (self.notice_panel, self.actions_panel, self.steps_panel):
            for child in panel.get_children():
                panel.remove(child)
                child.destroy()
        for child in self.body.get_children():
            self.body.remove(child)
            child.destroy()
        if step is not None:
            seen = False
            for key, name in INSTALL_STEPS:
                classes = ["step"]
                if key == step:
                    classes.append("active")
                    seen = True
                elif not seen:
                    classes.append("done")
                widget = label(name, *classes)
                widget.set_line_wrap(False)
                self.steps_panel.pack_start(widget, False, False, 0)
            self.steps_panel.show_all()
        self.body.pack_start(label(title, "title"), False, False, 0)
        if description:
            self.body.pack_start(label(description, "muted"), False, False, 0)

    def card(self, title, text):
        box = styled(Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8), "card")
        box.pack_start(label(title, "card-title"), False, False, 0)
        if text:
            box.pack_start(label(text, "muted"), False, False, 0)
        self.body.pack_start(box, False, False, 0)
        return box

    def notice(self, text, *classes):
        self.notice_panel.pack_start(label(text, "notice", *classes), False, False, 0)
        self.notice_panel.show_all()

    def actions(self, *buttons):
        for button in buttons:
            self.actions_panel.pack_start(button, False, False, 0)
        self.actions_panel.show_all()
        self.body.show_all()

    def ask(self, line):
        """A request to the supervisor; its answer, or "failed <why>"."""
        try:
            return self.supervisor.ask(line)
        except (OSError, ValueError) as error:
            return f"failed {error}"

    def in_background(self, work, done):
        """Runs `work` away from the window (a disk listing takes a while)."""
        def run():
            result = work()
            GLib.idle_add(lambda: (done(result), False)[1])
        threading.Thread(target=run, daemon=True).start()

    def set_busy(self, busy):
        self.busy = busy
        self.shutdown_button.set_sensitive(not busy)
        self.live_button.set_sensitive(not busy)

    # ── Installing ───────────────────────────────────────────────────────

    def show_welcome(self):
        self.clear("welcome", "Install OpenShell Native",
                   "OpenShell Native will be copied from this USB stick to a disk of this computer. "
                   "You choose the time zone, the keyboard, the disk and your account; then the copy takes a few minutes.")
        self.card("What it does", "The disk you choose is erased and gets OpenShell Native, with room for its updates "
                                  "and a recovery system. The computer then starts from it, without the USB stick.")
        start = self.button("Start", self.show_location, "primary")
        self.actions(start)
        start.grab_focus()

    def show_location(self):
        self.clear("location", "Where are you?", "The clock of your computer follows this time zone.", step="location")
        zones = model.time_zones()
        region, _, city = self.zone.partition("/")
        regions = Gtk.ComboBoxText()
        for name in zones:
            regions.append(name, name.replace("_", " "))
        regions.append("UTC", "UTC (no region)")
        cities = Gtk.ComboBoxText()

        def fill_cities(*_):
            cities.remove_all()
            chosen = regions.get_active_id()
            for name in zones.get(chosen, []):
                cities.append(name, name.replace("_", " ").replace("/", " · "))
            cities.set_sensitive(chosen != "UTC")
            if chosen in zones:
                if not cities.set_active_id(city):
                    cities.set_active(0)
        regions.connect("changed", fill_cities)
        if not regions.set_active_id(region if region in zones else "UTC"):
            regions.set_active(0)
        fill_cities()
        self.body.pack_start(label("Region", "small", "muted"), False, False, 0)
        self.body.pack_start(regions, False, False, 0)
        self.body.pack_start(label("City", "small", "muted"), False, False, 0)
        self.body.pack_start(cities, False, False, 0)

        def next_step():
            chosen = regions.get_active_id()
            self.zone = "UTC" if chosen == "UTC" or not cities.get_active_id() else f"{chosen}/{cities.get_active_id()}"
            self.show_keyboard()
        self.actions(self.button("Back", self.show_welcome), self.button("Continue", next_step, "primary"))

    @staticmethod
    def current_keyboard():
        try:
            query = subprocess.run(["setxkbmap", "-query"], capture_output=True, text=True, timeout=5).stdout
        except (OSError, subprocess.SubprocessError):
            return "us", ""
        values = dict(line.split(":", 1) for line in query.splitlines() if ":" in line)
        layout = values.get("layout", "us").strip().split(",")[0] or "us"
        variant = values.get("variant", "").strip().split(",")[0]
        return layout, variant

    @staticmethod
    def apply_keyboard(layout, variant):
        command = ["setxkbmap", "-layout", layout, "-option", ""]
        if variant:
            command += ["-variant", variant]
        try:
            subprocess.run(command, capture_output=True, timeout=5)
        except (OSError, subprocess.SubprocessError):
            pass

    def show_keyboard(self):
        self.clear("keyboard", "Your keyboard", "Choose the layout of your keyboard and try it below.", step="keyboard")
        layouts, variants = model.keyboard_layouts()
        if not layouts:
            layouts = [("us", "English (US)")]
        layout_box = Gtk.ComboBoxText()
        for name, description in layouts:
            layout_box.append(name, description)
        variant_box = Gtk.ComboBoxText()
        test = Gtk.Entry()
        test.set_placeholder_text("Type here to try the keyboard")

        def fill_variants(*_):
            variant_box.remove_all()
            variant_box.append("", "Default")
            for name, description in variants.get(layout_box.get_active_id(), []):
                variant_box.append(name, description)
            if not variant_box.set_active_id(self.variant if layout_box.get_active_id() == self.layout else ""):
                variant_box.set_active(0)

        def changed(*_):
            layout = layout_box.get_active_id()
            if layout:
                self.apply_keyboard(layout, variant_box.get_active_id() or "")
        layout_box.connect("changed", fill_variants)
        if not layout_box.set_active_id(self.layout):
            layout_box.set_active(0)
        fill_variants()
        layout_box.connect("changed", changed)
        variant_box.connect("changed", changed)
        for caption, widget in (("Layout", layout_box), ("Variant", variant_box)):
            self.body.pack_start(label(caption, "small", "muted"), False, False, 0)
            self.body.pack_start(widget, False, False, 0)
        self.body.pack_start(test, False, False, 0)

        def next_step():
            self.layout = layout_box.get_active_id() or "us"
            self.variant = variant_box.get_active_id() or ""
            self.show_disks()
        self.actions(self.button("Back", self.show_location), self.button("Continue", next_step, "primary"))

    def show_disks(self):
        self.clear("disk", "Choose the disk", "Looking at this computer's disks…", step="disk")
        self.body.show_all()
        self.set_busy(True)

        def listed(answer):
            self.set_busy(False)
            disks = model.parse_disks(answer)
            self.clear("disk", "Choose the disk", "OpenShell Native takes the whole disk you choose.", step="disk")
            listbox = Gtk.ListBox()
            listbox.set_selection_mode(Gtk.SelectionMode.SINGLE)
            for device, size in disks:
                row = Gtk.ListBoxRow()
                row.choice = (device, size)
                content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=5)
                name = model.disk_model(device) or "Disk"
                content.pack_start(label(f"{name} · {model.size_text(size)}", "card-title"), False, False, 0)
                content.pack_start(label(device, "muted", "small"), False, False, 0)
                row.add(content)
                listbox.add(row)
            self.body.pack_start(listbox, False, False, 0)
            if not disks:
                self.body.pack_start(label("No disk of 16 GB or more was found. Connect one and look again.", "notice"), False, False, 0)
            self.notice("Everything on the disk you choose is erased: its files, and any other system on it.")
            continue_button = self.button("Continue", lambda: choose(listbox), "primary")
            continue_button.set_sensitive(False)
            listbox.connect("row-selected", lambda _list, row: continue_button.set_sensitive(row is not None))
            self.actions(self.button("Back", self.show_keyboard), self.button("Look again", self.show_disks), continue_button)
            first = listbox.get_row_at_index(0)
            if first:
                listbox.select_row(first)
                listbox.grab_focus()

        def choose(listbox):
            row = listbox.get_selected_row()
            if row:
                self.disk = row.choice
                self.show_account()
        self.in_background(lambda: self.ask("install-list"), listed)

    def show_account(self, error=None):
        self.clear("account", "Your account", "The name others see, and the password that opens this computer.", step="account")
        if error:
            self.body.pack_start(label(error, "notice", "error"), False, False, 0)
        name = Gtk.Entry()
        name.set_placeholder_text("Your name")
        name.set_text(getattr(self, "account_name", ""))
        password = Gtk.Entry()
        confirm = Gtk.Entry()
        for entry, hint in ((password, "Password"), (confirm, "The password again")):
            entry.set_visibility(False)
            entry.set_input_purpose(Gtk.InputPurpose.PASSWORD)
            entry.set_placeholder_text(hint)
        for caption, widget in (("Name", name), ("Password", password), ("Confirm the password", confirm)):
            self.body.pack_start(label(caption, "small", "muted"), False, False, 0)
            self.body.pack_start(widget, False, False, 0)
        show_row = Gtk.Box(spacing=12)
        show_row.pack_start(label("Show the password", "small", "muted"), False, False, 0)
        visible = Gtk.Switch()
        visible.connect("notify::active", lambda switch, _: [entry.set_visibility(switch.get_active()) for entry in (password, confirm)])
        show_row.pack_start(visible, False, False, 0)
        self.body.pack_start(show_row, False, False, 0)
        why = label("", "small", "error")
        self.body.pack_start(why, False, False, 0)
        continue_button = self.button("Continue", lambda: next_step(), "primary")

        def check(*_):
            problem = ""
            if not model.valid_name(name.get_text()):
                problem = "Type your name (without : , or =)." if name.get_text() else ""
            elif not password.get_text():
                problem = ""
            elif not model.valid_password(password.get_text()):
                problem = "The password has a character it cannot have."
            elif confirm.get_text() and confirm.get_text() != password.get_text():
                problem = "The two passwords are not the same."
            why.set_text(problem)
            ready = model.valid_name(name.get_text()) and model.valid_password(password.get_text()) and confirm.get_text() == password.get_text()
            continue_button.set_sensitive(ready)
            return ready

        def next_step():
            if not check():
                return
            self.account_name = name.get_text().strip()
            self.account_password = password.get_text()
            password.set_text("")
            confirm.set_text("")
            self.show_summary()
        for entry in (name, password, confirm):
            entry.connect("changed", check)
        confirm.connect("activate", lambda *_: next_step())
        check()
        self.actions(self.button("Back", self.show_disks), continue_button)
        name.grab_focus()

    def show_summary(self):
        self.clear("summary", "Ready to install", "Check your choices before the disk is erased.", step="summary")
        device, size = self.disk
        self.card("Disk", f"{model.disk_model(device) or 'Disk'} · {model.size_text(size)} · {device}\nEverything on it is erased.")
        self.card("Time zone", self.zone.replace("_", " "))
        layouts, variants = model.keyboard_layouts()
        layout_name = dict(layouts).get(self.layout, self.layout)
        variant_name = dict(variants.get(self.layout, [])).get(self.variant, "")
        self.card("Keyboard", layout_name + (f" · {variant_name}" if variant_name else ""))
        self.card("Account", self.account_name)
        self.actions(self.button("Back", self.show_account), self.button("Install now", self.install, "primary"))

    def install(self):
        keyboard = self.layout + (f" {self.variant}" if self.variant else "")
        for line in (f"install-name {self.account_name}", f"install-password {self.account_password}",
                     f"install-keyboard {keyboard}", f"install-timezone {self.zone}"):
            answer = self.ask(line)
            if answer != "ok":
                what = line.split(" ", 1)[0].removeprefix("install-")
                self.show_account(f"The installer did not take the {what}: {answer}")
                return
        self.account_password = ""
        answer = self.ask(f"install {self.disk[0]}")
        if answer != "started":
            self.show_failure("install", answer)
            return
        self.follow("install-status", "Installing OpenShell Native", "Keep the USB stick connected. Do not turn the computer off.",
                    self.show_installed, lambda message: self.show_failure("install", message))

    def show_installed(self):
        self.clear("installed", "OpenShell Native is installed",
                   "Remove the USB stick, then restart: the computer starts from its disk, on your account's lock screen.")
        restart = self.button("Restart", lambda: self.power("reboot"), "primary")
        self.actions(self.button("Shut down", lambda: self.power("poweroff")), restart)
        restart.grab_focus()

    # ── Updating ─────────────────────────────────────────────────────────

    def prepare_update(self):
        self.release = model.medium_release()
        if not model.medium_signed():
            self.clear("unsigned", "This USB stick cannot update",
                       "It was made without OpenShell Native's release key, so an installed system cannot check what it would get. "
                       "Make the stick again with the release key, or install anew.")
            self.actions(self.button("Try OpenShell Native", self.try_live))
            return
        self.scan()

    def scan(self):
        self.clear("scan", "Finding installed systems", "Disks are looked at without changing them.")
        self.body.show_all()
        self.set_busy(True)

        def listed(answer):
            self.set_busy(False)
            self.show_selection(model.parse_systems(answer))
        self.in_background(lambda: self.ask("usb-update-list"), listed)

    def show_selection(self, systems):
        self.clear("selection", "Choose your installation", "Update OpenShell Native from this USB stick, or put back the release from before the last update.")
        self.card("USB release · OpenShell Native " + (self.release or "?"), "")
        listbox = Gtk.ListBox()
        listbox.set_selection_mode(Gtk.SelectionMode.SINGLE)
        for system in systems:
            row = Gtk.ListBoxRow()
            row.choice = system
            content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=5)
            content.pack_start(label("OpenShell Native " + system["release"], "card-title"), False, False, 0)
            text = f"{model.disk_model(system['device']) or 'Disk'} · {model.size_text(system['bytes'])} · {system['device']}"
            if system["previous"]:
                text += f"\nBefore the last update: {system['previous']}"
            content.pack_start(label(text, "muted", "small"), False, False, 0)
            row.add(content)
            listbox.add(row)
        self.body.pack_start(listbox, False, False, 0)
        if not systems:
            self.body.pack_start(label("No OpenShell Native installation was found on this computer's disks.", "notice"), False, False, 0)
        continue_button = self.button("Continue", lambda: choose(), "primary")
        continue_button.set_sensitive(False)
        listbox.connect("row-selected", lambda _list, row: continue_button.set_sensitive(row is not None))

        def choose():
            row = listbox.get_selected_row()
            if row:
                self.target = row.choice
                self.show_review()
        self.actions(self.button("Look again", self.scan), continue_button)
        first = listbox.get_row_at_index(0)
        if first:
            listbox.select_row(first)
            listbox.grab_focus()

    def show_review(self, restoring=False):
        target = self.target
        if restoring:
            self.clear("review", f"Put back OpenShell Native {target['previous']}?",
                       "The release from before the last update starts again; if it does not, the present one comes back.")
        else:
            self.clear("review", "Ready to update", "Check the installation before continuing. Your files and settings are kept.")
        self.card("Installed · OpenShell Native " + target["release"],
                  f"{model.disk_model(target['device']) or 'Disk'} · {model.size_text(target['bytes'])} · {target['device']}")
        if not restoring:
            self.card("From this USB · OpenShell Native " + self.release, "")
            order = model.compare_releases(self.release, target["release"])
            if order == 0:
                self.notice("This USB stick has the release that is installed: continuing installs it again.")
            elif order < 0:
                self.notice("This USB stick has an older release than the installed one: continuing goes back to it.")
        self.body.pack_start(label("The new release starts at the next start of the computer. If it does not start, "
                                   "the computer goes back to the one it has now. Partitions are not formatted.", "muted", "small"), False, False, 0)
        buttons = [self.button("Back", self.scan)]
        if restoring:
            buttons.append(self.button("Put it back", lambda: self.update(True), "primary"))
        else:
            if target["previous"]:
                buttons.append(self.button("Put back the previous release…", lambda: self.show_review(True)))
            buttons.append(self.button("Update now", lambda: self.update(False), "primary"))
        self.actions(*buttons)

    def update(self, restoring):
        device = self.target["device"]
        answer = self.ask(f"usb-update-restore {device}" if restoring else f"usb-update {device}")
        if answer != "started":
            self.show_failure("update", answer)
            return
        title = "Putting back the previous release" if restoring else "Updating OpenShell Native"
        self.follow("usb-update-status", title, "Keep the USB stick connected. Do not turn the computer off.",
                    lambda: self.show_updated(restoring), lambda message: self.show_failure("update", message))

    def show_updated(self, restoring):
        release = self.target["previous"] if restoring else self.release
        self.clear("updated", "OpenShell Native is ready",
                   f"Remove the USB stick, then restart: OpenShell Native {release} starts, and is kept once it has started.")
        restart = self.button("Restart", lambda: self.power("reboot"), "primary")
        self.actions(self.button("Shut down", lambda: self.power("poweroff")), restart)
        restart.grab_focus()

    # ── Both ─────────────────────────────────────────────────────────────

    def follow(self, status_request, title, description, success, failure):
        """Shows a job's progress until it ends."""
        self.clear("progress", title, description)
        bar = Gtk.ProgressBar()
        step = label("Starting…", "muted")
        self.body.pack_start(bar, False, False, 0)
        self.body.pack_start(step, False, False, 0)
        self.body.show_all()
        self.set_busy(True)

        def poll():
            state, percent, message = model.parse_status(self.ask(status_request))
            if state == "running":
                bar.set_fraction(percent / 100)
                step.set_text(message or "Working…")
                return True
            self.set_busy(False)
            self.poll_source = None
            if state == "done":
                success()
            else:
                failure(message)
            return False
        self.poll_source = GLib.timeout_add(POLL_MS, poll)

    def show_failure(self, what, message):
        if what == "install":
            self.clear("failure", "The installation stopped",
                       "The disk may be partly written: installing again starts it over. Read why below.")
        else:
            self.clear("failure", "The update stopped",
                       "The installed system keeps starting the release it has. Read why below.")
        self.body.pack_start(label(message.removeprefix("failed ").strip() or "It stopped.", "card", "error"), False, False, 0)
        again = self.show_summary if what == "install" and self.disk else (self.scan if what == "update" else self.show_welcome)
        self.actions(self.button("Try again", again, "primary"))

    def request_shutdown(self):
        if self.busy:
            return
        back = self.page
        self.clear("shutdown", "Shut down the computer?", "Remove the USB stick only after the computer has switched off.")
        resume = {"welcome": self.show_welcome}.get(back, self.show_welcome if self.mode == "install" else self.scan)
        self.actions(self.button("Back", resume), self.button("Shut down", lambda: self.power("poweroff"), "primary"))

    def power(self, verb):
        if self.ask(f"power {verb}") == "ok":
            self.set_busy(True)
            self.clear("power", "Shutting down…" if verb == "poweroff" else "Restarting…")
            self.body.show_all()

    def try_live(self):
        """Leaves the setup: the live Desktop starts instead (installed-session.sh)."""
        if not self.busy:
            Gtk.main_quit()


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "install"
    if mode not in ("install", "update"):
        sys.exit(__doc__)
    window = SetupWindow(mode)
    window.present()
    Gtk.main()


if __name__ == "__main__":
    main()
