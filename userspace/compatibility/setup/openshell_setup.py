#!/usr/bin/python3
"""The live medium's setup screen: what its boot menu's "Install OpenShell
Native on this computer" and "Update installed OpenShell Native (keep
files)" open instead of the Desktop, as OpenShell Live's do (its installer
and its updater, update_gui.py, whose colours these are).

    openshell_setup.py install    the installer, one step at a time, as
                                  Calamares asks on Live: location (time
                                  zone), keyboard, disk, account, summary,
                                  then the copy
    openshell_setup.py update     the installations on this computer, the
                                  review, then the update (or the previous
                                  release back)

The steps are listed on the left, done, current and to come; each one is a
card with its question, and Back and Continue under it (Enter continues,
Escape goes back). It runs as the session's user under the session
supervisor, which does the work as root (install.rs, usb_update.rs) and
powers the computer off or restarts it (power.rs); setup_model.py speaks to
it. Leaving it with "Try OpenShell Native" starts the live Desktop instead
(installed-session.sh)."""
import os
import subprocess
import sys
import threading
from datetime import datetime, timezone

import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, GLib, Gtk, Pango

try:
    from zoneinfo import ZoneInfo
except ImportError:  # An older Python: no local time next to the zone.
    ZoneInfo = None

# Isolated (python3 -I): its own folder is not on the path by itself.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import setup_model as model  # noqa: E402

# OpenShell Live's updater's colours (update_gui.py), the splash's wordmark.
CSS = b"""
window { background-color: #070d16; color: #e7edf6; font-family: 'Noto Sans', sans-serif; font-size: 14px; }
label { color: #e7edf6; }
.sidebar { background-color: #0a121d; border-right: 1px solid #1a283a; padding: 40px 30px 28px 34px; }
.main { background-color: #070d16;
        background-image: radial-gradient(circle farthest-corner at 85% 0%, rgba(122, 162, 247, 0.10), rgba(7, 13, 22, 0) 60%); }
.wordmark { font-family: 'Cascadia Code', 'Noto Sans Mono', monospace; font-size: 26px; font-weight: bold; letter-spacing: 3px; }
.wordmark.open { color: #7aa2f7; }
.wordmark.shell { color: #d4d4d4; }
.kicker { color: #8494ac; font-size: 11px; font-weight: bold; letter-spacing: 2px; }
.dot { min-width: 30px; min-height: 30px; border-radius: 16px; border: 2px solid #2c3d54; color: #8494ac; font-size: 13px; font-weight: bold; }
.dot.active { background-color: #88aaff; border-color: #88aaff; color: #0b172a; }
.dot.done { background-color: #143529; border-color: #4fb48a; color: #91e0bf; }
.rail { min-width: 2px; min-height: 22px; margin: 3px 0 3px 16px; background-color: #22324a; }
.rail.done { background-color: #4fb48a; }
.step-name { font-size: 15px; font-weight: bold; color: #8494ac; }
.step-name.active { color: #e7edf6; }
.step-name.done { color: #b8c6d8; }
.step-what { font-size: 12px; color: #66768e; }
.panel { background-color: #0e1824; border: 1px solid #1f3046; border-radius: 18px; padding: 34px 38px 26px 38px;
         box-shadow: 0 24px 60px rgba(0, 0, 0, 0.45); }
.eyebrow { color: #88aaff; font-size: 12px; font-weight: bold; letter-spacing: 2px; }
.title { font-size: 28px; font-weight: bold; }
.muted { color: #a2b1c5; }
.small { font-size: 12px; }
.caption { color: #8ea0b8; font-size: 12px; font-weight: bold; letter-spacing: 1px; }
.fact { background-color: #121e2d; border: 1px solid #22344b; border-radius: 14px; padding: 16px; }
.fact-title { font-size: 15px; font-weight: bold; color: #c9d8f5; }
.callout { background-color: #2a1820; border: 1px solid #6b3442; border-radius: 12px; padding: 13px 16px; color: #ffc9cf; }
.callout.info { background-color: #142238; border-color: #34507e; color: #c9d8f5; }
.error { color: #ffb2ba; }
.actions { border-top: 1px solid #1f3046; padding-top: 20px; margin-top: 8px; }
button { background-color: #162435; background-image: none; color: #e7edf6; border: 1px solid #2f4560; border-radius: 10px;
         padding: 10px 22px; box-shadow: none; text-shadow: none; }
button:hover { background-color: #20344c; border-color: #799ce3; }
button:focus { outline-color: #9bb7ff; outline-width: 2px; outline-style: solid; outline-offset: 2px; }
button:disabled { opacity: 0.45; }
button.primary { background-color: #88aaff; border-color: #88aaff; color: #0b172a; font-weight: bold; }
button.primary label { color: #0b172a; }
button.primary:hover { background-color: #a4bfff; }
button.flat, button.link { background-color: transparent; border-color: transparent; }
button.flat:hover { background-color: #162435; border-color: #2f4560; }
button.link { padding: 4px 6px; color: #a9c1ff; }
button.link label { color: #a9c1ff; }
button.link:hover { background-color: transparent; border-color: transparent; }
button.link:hover label { color: #d2defc; }
button.side { padding: 6px 0; background-color: transparent; border-color: transparent; }
button.side label { color: #8ea0b8; }
button.side:hover label { color: #e7edf6; }
entry { background-color: #0a1420; background-image: none; color: #e7edf6; border: 1px solid #2f4560; border-radius: 10px;
        padding: 10px 14px; box-shadow: none; }
entry:focus { border-color: #88aaff; }
scrolledwindow.choices { border: 1px solid #1f3046; border-radius: 12px; background-color: #0a1420; }
list { background-color: transparent; }
list.choices row { padding: 10px 14px; margin: 3px 5px; border-radius: 9px; border: 1px solid transparent; }
list.choices row:hover { background-color: #13202f; }
list.choices row:selected { background-color: #1a2c45; border-color: #4f74c4; }
list.cards row { padding: 16px 18px; margin-bottom: 10px; border-radius: 12px; background-color: #121e2d; border: 1px solid #263850; }
list.cards row:hover { border-color: #3d5778; }
list.cards row:selected { background-color: #182b45; border-color: #88aaff; }
row:focus { outline-color: #9bb7ff; }
.radio { min-width: 14px; min-height: 14px; border-radius: 9px; border: 2px solid #4a5f7d; }
row:selected .radio { background-color: #88aaff; border-color: #88aaff; box-shadow: inset 0 0 0 3px #182b45; }
.row-title { font-size: 15px; font-weight: bold; }
.badge { background-color: #1b2b40; color: #a9c1ff; border-radius: 6px; padding: 3px 9px; font-size: 11px; font-weight: bold; letter-spacing: 1px; }
.review-row { padding: 12px 0; border-bottom: 1px solid #1c2b3f; }
switch { background-color: #33445d; border-radius: 14px; }
switch:checked { background-color: #88aaff; }
switch slider { background-color: #e7edf6; border-radius: 12px; }
progressbar trough { background-color: #1c2a40; background-image: none; border: none; min-height: 10px; border-radius: 6px; }
progressbar progress { background-color: #88aaff; background-image: none; border: none; min-height: 10px; border-radius: 6px; }
.percent { font-size: 46px; font-weight: bold; color: #e7edf6; }
.seal { min-width: 76px; min-height: 76px; border-radius: 40px; font-size: 38px; font-weight: bold; }
.seal.good { background-color: #143529; color: #91e0bf; border: 2px solid #4fb48a; }
.seal.bad { background-color: #3a1b23; color: #ffb2ba; border: 2px solid #a14a5a; }
"""

# The steps each mode walks through, as the sidebar lists them: (page, name, what).
INSTALL_STEPS = [
    ("location", "Location", "Your time zone"),
    ("keyboard", "Keyboard", "Its layout"),
    ("disk", "Disk", "Where it goes"),
    ("account", "Account", "Name and password"),
    ("summary", "Summary", "Check, then confirm"),
    ("install", "Install", "The copy"),
]
UPDATE_STEPS = [
    ("selection", "Installation", "The system to update"),
    ("review", "Review", "Check, then confirm"),
    ("update", "Update", "The copy"),
]
# Every step done (the work ended well).
FINISHED = "finished"
# The status the supervisor's jobs are polled at, in milliseconds.
POLL_MS = 500
# A layout picked with the arrow keys is applied once the keys rest.
KEYBOARD_DELAY_MS = 300
# The panel's width, and the height of its scrolling lists.
PANEL_WIDTH = 720
LIST_HEIGHT = 300


def styled(widget, *classes):
    for name in classes:
        widget.get_style_context().add_class(name)
    return widget


def label(text, *classes):
    """A paragraph, wrapped."""
    widget = Gtk.Label(label=str(text), xalign=0)
    widget.set_line_wrap(True)
    widget.set_line_wrap_mode(Pango.WrapMode.WORD_CHAR)
    widget.set_max_width_chars(70)
    return styled(widget, *classes)


def word(text, *classes, ellipsize=False):
    """A label on one line, cut with an ellipsis where asked (not on
    letter-spaced text, which GTK 3 measures short)."""
    widget = Gtk.Label(label=str(text), xalign=0)
    if ellipsize:
        widget.set_ellipsize(Pango.EllipsizeMode.END)
    return styled(widget, *classes)


def local_time(zone):
    """The time now in a zone, as the clock will show it, or "" when unknown."""
    try:
        if zone == "UTC":
            return datetime.now(timezone.utc).strftime("%H:%M")
        with open(model.ZONEINFO / zone, "rb") as data:
            return datetime.now(ZoneInfo.from_file(data, key=zone)).strftime("%H:%M")
    except (AttributeError, OSError, ValueError):
        return ""


class SetupWindow(Gtk.Window):
    def __init__(self, mode, supervisor=None, fullscreen=True):
        super().__init__(title="OpenShell Native · " + ("Install" if mode == "install" else "Update from USB"))
        self.mode = mode
        self.steps = INSTALL_STEPS if mode == "install" else UPDATE_STEPS
        self.supervisor = supervisor or model.Supervisor()
        self.busy = False
        self.page = None
        self.poll_source = None
        self.keyboard_source = None
        self.back_callback = None
        # The card's primary button (Enter, a list's double click) and its
        # search box (what is typed over a list goes there).
        self.primary = None
        self.search = None
        # The installer's answers, and the updater's choice.
        self.zone = model.current_zone() or "UTC"
        self.layout, self.variant = self.current_keyboard()
        self.disk = None
        self.target = None
        self.account_name = ""
        self.account_password = ""
        self.set_default_size(1280, 800)
        self.set_decorated(False)
        self.connect("delete-event", lambda *_: True)
        self.connect("key-press-event", self.key_pressed)
        provider = Gtk.CssProvider()
        provider.load_from_data(CSS)
        Gtk.StyleContext.add_provider_for_screen(Gdk.Screen.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

        root = Gtk.Box()
        self.add(root)
        root.pack_start(self.build_sidebar(), False, False, 0)
        main = styled(Gtk.Box(orientation=Gtk.Orientation.VERTICAL), "main")
        root.pack_start(main, True, True, 0)
        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        main.pack_start(scroll, True, True, 0)
        holder = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        holder.set_border_width(28)
        scroll.add(holder)

        self.panel = styled(Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0), "panel")
        self.panel.set_size_request(PANEL_WIDTH, -1)
        self.panel.set_halign(Gtk.Align.CENTER)
        self.panel.set_valign(Gtk.Align.CENTER)
        holder.pack_start(self.panel, True, False, 0)
        self.seal_slot = Gtk.Box()
        self.eyebrow = word("", "eyebrow")
        self.heading = label("", "title")
        self.description = label("", "muted")
        self.body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=14)
        self.notice_panel = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.actions_panel = styled(Gtk.Box(spacing=10), "actions")
        self.panel.pack_start(self.seal_slot, False, False, 0)
        self.panel.pack_start(self.eyebrow, False, False, 0)
        self.panel.pack_start(self.heading, False, False, 6)
        self.panel.pack_start(self.description, False, False, 0)
        self.panel.pack_start(self.body, False, False, 18)
        self.panel.pack_start(self.notice_panel, False, False, 0)
        self.panel.pack_start(self.actions_panel, False, False, 10)

        if fullscreen:
            self.fullscreen()
        self.show_all()
        if mode == "install":
            self.show_welcome()
        else:
            self.prepare_update()

    def build_sidebar(self):
        sidebar = styled(Gtk.Box(orientation=Gtk.Orientation.VERTICAL), "sidebar")
        sidebar.set_size_request(300, -1)
        mark = Gtk.Box()
        mark.pack_start(word("OPEN", "wordmark", "open"), False, False, 0)
        mark.pack_start(word("SHELL", "wordmark", "shell"), False, False, 0)
        sidebar.pack_start(mark, False, False, 0)
        sidebar.pack_start(word("NATIVE · " + ("INSTALLER" if self.mode == "install" else "UPDATE FROM USB"), "kicker"), False, False, 6)
        self.steps_panel = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        sidebar.pack_start(self.steps_panel, False, False, 34)

        bottom = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        hint = "Nothing changes on this computer until you confirm." if self.mode == "install" else \
            "Your files, accounts, settings and installed apps are kept."
        hint = label(hint, "small", "muted")
        hint.set_max_width_chars(34)
        bottom.pack_start(hint, False, False, 8)
        self.live_button = self.button("Try OpenShell Native", self.try_live, "side")
        self.shutdown_button = self.button("Shut down", self.request_shutdown, "side")
        for button in (self.live_button, self.shutdown_button):
            button.set_halign(Gtk.Align.START)
            bottom.pack_start(button, False, False, 0)
        sidebar.pack_end(bottom, False, False, 0)
        return sidebar

    # ── Pieces ───────────────────────────────────────────────────────────

    def button(self, text, callback, *classes):
        widget = styled(Gtk.Button(label=text), *classes)
        widget.connect("clicked", lambda *_: callback())
        return widget

    def show_steps(self, current):
        """The sidebar's steps: those before `current` done, it active."""
        for child in self.steps_panel.get_children():
            self.steps_panel.remove(child)
            child.destroy()
        pages = [page for page, _, _ in self.steps]
        index = len(pages) if current == FINISHED else (pages.index(current) if current in pages else -1)
        for number, (_, name, what) in enumerate(self.steps):
            state = "done" if number < index else ("active" if number == index else "todo")
            row = Gtk.Box(spacing=14)
            dot = word("✓" if state == "done" else str(number + 1), "dot", state)
            dot.set_xalign(0.5)
            dot.set_valign(Gtk.Align.CENTER)
            row.pack_start(dot, False, False, 0)
            texts = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=1)
            texts.set_valign(Gtk.Align.CENTER)
            texts.pack_start(word(name, "step-name", state), False, False, 0)
            texts.pack_start(word(what, "step-what"), False, False, 0)
            row.pack_start(texts, True, True, 0)
            self.steps_panel.pack_start(row, False, False, 0)
            if number < len(self.steps) - 1:
                rail = styled(Gtk.Box(), "rail", "done" if number < index else "todo")
                rail.set_halign(Gtk.Align.START)
                self.steps_panel.pack_start(rail, False, False, 0)
        self.steps_panel.show_all()

    def clear(self, page, title, description=None, step=None, eyebrow=None):
        """A new card: its title and description, the sidebar's steps."""
        self.page = page
        self.back_callback = None
        self.primary = None
        self.search = None
        for panel in (self.seal_slot, self.notice_panel, self.actions_panel, self.body):
            for child in panel.get_children():
                panel.remove(child)
                child.destroy()
        pages = [key for key, _, _ in self.steps]
        if step in pages:
            self.show_steps(step)
            eyebrow = eyebrow or f"STEP {pages.index(step) + 1} OF {len(pages)}"
        elif step is not None:
            self.show_steps(step)
        self.eyebrow.set_text(eyebrow or "")
        self.eyebrow.set_visible(bool(eyebrow))
        self.heading.set_text(title)
        self.description.set_text(description or "")
        self.description.set_visible(bool(description))
        self.seal_slot.set_visible(False)
        self.actions_panel.set_visible(False)

    def caption(self, text):
        self.body.pack_start(word(text.upper(), "caption"), False, False, 0)

    def callout(self, text, *classes):
        box = label(text, "callout", *classes)
        self.body.pack_start(box, False, False, 0)
        return box

    def notice(self, text, *classes):
        self.notice_panel.pack_start(label(text, "callout", *classes), False, False, 6)
        self.notice_panel.show_all()

    def actions(self, *buttons, back=None):
        """The card's buttons: Back on the left, the others on the right with
        the primary one last; Enter presses the primary one, Escape Back."""
        self.back_callback = back
        if back:
            self.actions_panel.pack_start(self.button("Back", back, "flat"), False, False, 0)
        for button in reversed(buttons):
            self.actions_panel.pack_end(button, False, False, 0)
        for button in buttons:
            if button.get_style_context().has_class("primary"):
                button.set_can_default(True)
                self.set_default(button)
                self.primary = button
        self.actions_panel.show_all()
        self.actions_panel.set_visible(bool(back or buttons))
        self.body.show_all()

    def press_primary(self):
        if self.primary is not None and self.primary.get_sensitive():
            self.primary.clicked()

    def choice_list(self, css="choices", height=None):
        """A list of choices (one selected), scrolling when `height` is given."""
        listbox = styled(Gtk.ListBox(), css)
        listbox.set_selection_mode(Gtk.SelectionMode.SINGLE)
        listbox.connect("row-activated", lambda *_: self.press_primary())
        if height is None:
            return listbox, listbox
        scroll = styled(Gtk.ScrolledWindow(), "choices")
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_size_request(-1, height)
        scroll.add(listbox)
        listbox.set_focus_vadjustment(scroll.get_vadjustment())
        return listbox, scroll

    @staticmethod
    def choice_row(choice, title, detail=None, badge=None):
        row = Gtk.ListBoxRow()
        row.choice = choice
        line = Gtk.Box(spacing=14)
        radio = styled(Gtk.Box(), "radio")
        radio.set_valign(Gtk.Align.CENTER)
        line.pack_start(radio, False, False, 0)
        texts = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=3)
        texts.pack_start(word(title, "row-title", ellipsize=True), False, False, 0)
        if detail:
            texts.pack_start(word(detail, "muted", "small", ellipsize=True), False, False, 0)
        line.pack_start(texts, True, True, 0)
        if badge:
            tag = word(badge, "badge")
            tag.set_valign(Gtk.Align.CENTER)
            line.pack_end(tag, False, False, 0)
        row.add(line)
        return row

    @staticmethod
    def scroll_to(listbox, row):
        """Shows the selected row in its list once the list has its size."""
        def scroll():
            adjustment = listbox.get_focus_vadjustment()
            allocation = row.get_allocation()
            if adjustment is not None and allocation.y >= 0:
                adjustment.set_value(max(0, allocation.y - allocation.height * 2))
            return False
        GLib.timeout_add(80, scroll)

    def search_entry(self, hint, listbox, text_of):
        """An entry that keeps the list's rows whose text has what is typed."""
        entry = Gtk.Entry()
        entry.set_placeholder_text(hint)

        def keep(row):
            typed = entry.get_text().strip().lower()
            return not typed or typed in text_of(row)
        listbox.set_filter_func(keep)
        entry.connect("changed", lambda *_: listbox.invalidate_filter())
        self.search = entry
        return entry

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

    def key_pressed(self, _window, event):
        if event.keyval == Gdk.KEY_Escape and self.back_callback and not self.busy:
            self.back_callback()
            return True
        # Typing over a list searches it.
        code = Gdk.keyval_to_unicode(event.keyval)
        modifiers = Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.MOD1_MASK
        if self.search is not None and code and chr(code).isprintable() and \
                not event.state & modifiers and not isinstance(self.get_focus(), Gtk.Entry):
            self.search.grab_focus_without_selecting()
            self.search.set_text(self.search.get_text() + chr(code))
            self.search.set_position(-1)
            return True
        return False

    # ── Installing ───────────────────────────────────────────────────────

    def show_welcome(self):
        self.clear("welcome", "Install OpenShell Native",
                   "OpenShell Native is copied from this USB stick to a disk of this computer. You choose the time zone, "
                   "the keyboard, the disk and your account; then the copy takes a few minutes.",
                   step="welcome", eyebrow="WELCOME")
        facts = Gtk.Box(spacing=12, homogeneous=True)
        for title, text in (
                ("A whole disk", "The disk you choose is erased and gets OpenShell Native, room for its updates and a recovery system."),
                ("Safe updates", "Each new release starts on trial: if it does not start, the computer goes back to the one it had."),
                ("No stick after", "Then the computer starts from its disk, on your account's lock screen.")):
            fact = styled(Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6), "fact")
            fact.pack_start(label(title, "fact-title"), False, False, 0)
            text = label(text, "muted", "small")
            text.set_max_width_chars(24)
            fact.pack_start(text, False, False, 0)
            facts.pack_start(fact, True, True, 0)
        self.body.pack_start(facts, False, False, 0)
        start = self.button("Start", self.show_location, "primary")
        self.actions(start)
        start.grab_focus()

    def show_location(self):
        self.clear("location", "Where are you?", "The computer's clock follows the time zone you choose.", step="location")
        zones = model.time_zones()
        listbox, scroll = self.choice_list(height=LIST_HEIGHT)
        chosen_row = None
        for region, cities in zones.items():
            for city in cities:
                zone = f"{region}/{city}"
                row = self.choice_row(zone, city.replace("_", " ").replace("/", " · "), region.replace("_", " "))
                row.search_text = zone.replace("_", " ").lower()
                listbox.add(row)
                if zone == self.zone:
                    chosen_row = row
        row = self.choice_row("UTC", "UTC", "Coordinated Universal Time, no region")
        row.search_text = "utc coordinated universal time"
        listbox.add(row)
        chosen_row = chosen_row or row
        search = self.search_entry("Search for a city or region", listbox, lambda row: row.search_text)
        chosen = word("", "muted")

        def selected(_list, row):
            if row is None:
                return
            self.zone = row.choice
            time = local_time(self.zone)
            chosen.set_text(f"{self.zone.replace('_', ' ')}" + (f"  ·  {time} there now" if time else ""))
        listbox.connect("row-selected", selected)
        self.body.pack_start(search, False, False, 0)
        self.body.pack_start(scroll, False, False, 0)
        self.body.pack_start(chosen, False, False, 0)
        self.actions(self.button("Continue", self.show_keyboard, "primary"), back=self.show_welcome)
        listbox.select_row(chosen_row)
        self.scroll_to(listbox, chosen_row)
        chosen_row.grab_focus()

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

    def keyboard_later(self, layout, variant):
        """Applies a layout once the choice rests, away from the window."""
        if self.keyboard_source:
            GLib.source_remove(self.keyboard_source)

        def apply():
            self.keyboard_source = None
            threading.Thread(target=self.apply_keyboard, args=(layout, variant), daemon=True).start()
            return False
        self.keyboard_source = GLib.timeout_add(KEYBOARD_DELAY_MS, apply)

    def show_keyboard(self):
        self.clear("keyboard", "Your keyboard", "Choose its layout, then try it in the box below.", step="keyboard")
        layouts, variants = model.keyboard_layouts()
        if not layouts:
            layouts = [("us", "English (US)")]
        layout_list, layout_scroll = self.choice_list(height=LIST_HEIGHT - 40)
        chosen_layout = None
        for name, description in layouts:
            row = self.choice_row(name, description, name)
            row.search_text = f"{description} {name}".lower()
            layout_list.add(row)
            if name == self.layout:
                chosen_layout = row
        variant_list, variant_scroll = self.choice_list(height=LIST_HEIGHT - 40)
        search = self.search_entry("Search for a language or country", layout_list, lambda row: row.search_text)
        test = Gtk.Entry()
        test.set_placeholder_text("Type here to try the keyboard")

        def fill_variants(layout):
            for child in variant_list.get_children():
                variant_list.remove(child)
                child.destroy()
            chosen = None
            for name, description in [("", "Default")] + variants.get(layout, []):
                row = self.choice_row(name, description)
                variant_list.add(row)
                if layout == self.layout and name == self.variant:
                    chosen = row
            variant_list.show_all()
            chosen = chosen or variant_list.get_row_at_index(0)
            variant_list.select_row(chosen)
            self.scroll_to(variant_list, chosen)

        def layout_selected(_list, row):
            if row is None:
                return
            fill_variants(row.choice)

        def variant_selected(_list, row):
            layout = layout_list.get_selected_row()
            if row is None or layout is None:
                return
            self.layout, self.variant = layout.choice, row.choice
            self.keyboard_later(self.layout, self.variant)
        layout_list.connect("row-selected", layout_selected)
        variant_list.connect("row-selected", variant_selected)

        self.body.pack_start(search, False, False, 0)
        columns = Gtk.Box(spacing=14, homogeneous=True)
        for title, widget in (("Layout", layout_scroll), ("Variant", variant_scroll)):
            column = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
            column.pack_start(word(title.upper(), "caption"), False, False, 0)
            column.pack_start(widget, False, False, 0)
            columns.pack_start(column, True, True, 0)
        self.body.pack_start(columns, False, False, 0)
        self.caption("Try it")
        self.body.pack_start(test, False, False, 0)
        self.actions(self.button("Continue", self.show_disks, "primary"), back=self.show_location)
        chosen_layout = chosen_layout or layout_list.get_row_at_index(0)
        layout_list.select_row(chosen_layout)
        self.scroll_to(layout_list, chosen_layout)
        chosen_layout.grab_focus()

    def show_disks(self):
        self.clear("disk", "Choose the disk", "Looking at this computer's disks…", step="disk")
        spinner = Gtk.Spinner()
        spinner.start()
        spinner.set_halign(Gtk.Align.START)
        self.body.pack_start(spinner, False, False, 0)
        self.body.show_all()
        self.set_busy(True)

        def listed(answer):
            self.set_busy(False)
            disks = model.parse_disks(answer)
            self.clear("disk", "Choose the disk", "OpenShell Native takes the whole disk you choose.", step="disk")
            listbox, _ = self.choice_list("cards")
            for device, size in disks:
                name = model.disk_model(device) or "Disk"
                listbox.add(self.choice_row((device, size), f"{name}  ·  {model.size_text(size)}", device, model.disk_kind(device)))
            self.body.pack_start(listbox, False, False, 0)
            if disks:
                self.notice("Everything on the disk you choose is erased: its files, and any other system on it.")
            else:
                self.callout("No disk of 16 GB or more was found. Connect one and look again.", "info")
            continue_button = self.button("Continue", lambda: choose(listbox), "primary")
            continue_button.set_sensitive(False)
            listbox.connect("row-selected", lambda _list, row: continue_button.set_sensitive(row is not None))
            self.actions(self.button("Look again", self.show_disks), continue_button, back=self.show_keyboard)
            chosen = None
            for row in listbox.get_children():
                if self.disk and row.choice[0] == self.disk[0]:
                    chosen = row
            chosen = chosen or listbox.get_row_at_index(0)
            if chosen:
                listbox.select_row(chosen)
                chosen.grab_focus()

        def choose(listbox):
            row = listbox.get_selected_row()
            if row:
                self.disk = row.choice
                self.show_account()
        self.in_background(lambda: self.ask("install-list"), listed)

    def show_account(self, error=None):
        self.clear("account", "Your account", "The name others see, and the password that opens this computer.", step="account")
        if error:
            self.callout(error)
        form = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        form.set_size_request(460, -1)
        form.set_halign(Gtk.Align.START)
        name = Gtk.Entry()
        name.set_placeholder_text("Your name")
        name.set_text(self.account_name)
        password = Gtk.Entry()
        confirm = Gtk.Entry()
        for entry, hint in ((password, "Password"), (confirm, "The password again")):
            entry.set_visibility(False)
            entry.set_input_purpose(Gtk.InputPurpose.PASSWORD)
            entry.set_placeholder_text(hint)
        for caption, widget in (("Name", name), ("Password", password), ("Confirm the password", confirm)):
            form.pack_start(word(caption.upper(), "caption"), False, False, 4)
            form.pack_start(widget, False, False, 0)
        show_row = Gtk.Box(spacing=12)
        visible = Gtk.Switch()
        visible.connect("notify::active", lambda switch, _: [entry.set_visibility(switch.get_active()) for entry in (password, confirm)])
        show_row.pack_start(visible, False, False, 0)
        show_row.pack_start(word("Show the password", "muted", "small"), False, False, 0)
        form.pack_start(show_row, False, False, 8)
        why = label("", "small", "error")
        form.pack_start(why, False, False, 0)
        self.body.pack_start(form, False, False, 0)
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
        name.connect("activate", lambda *_: password.grab_focus())
        password.connect("activate", lambda *_: confirm.grab_focus())
        confirm.connect("activate", lambda *_: next_step())
        check()
        self.actions(continue_button, back=self.show_disks)
        name.grab_focus()

    def show_summary(self):
        self.clear("summary", "Ready to install", "Check your choices before the disk is erased.", step="summary")
        device, size = self.disk
        layouts, variants = model.keyboard_layouts()
        layout_name = dict(layouts).get(self.layout, self.layout)
        variant_name = dict(variants.get(self.layout, [])).get(self.variant, "")
        time = local_time(self.zone)
        review = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        for caption, value, change in (
                ("Time zone", self.zone.replace("_", " ") + (f"  ·  {time} now" if time else ""), self.show_location),
                ("Keyboard", layout_name + (f"  ·  {variant_name}" if variant_name else ""), self.show_keyboard),
                ("Disk", f"{model.disk_model(device) or 'Disk'}  ·  {model.size_text(size)}  ·  {device}", self.show_disks),
                ("Account", self.account_name, self.show_account)):
            row = styled(Gtk.Box(spacing=16), "review-row")
            title = word(caption.upper(), "caption")
            title.set_size_request(130, -1)
            row.pack_start(title, False, False, 0)
            row.pack_start(word(value, ellipsize=True), True, True, 0)
            row.pack_end(self.button("Change", change, "link"), False, False, 0)
            review.pack_start(row, False, False, 0)
        self.body.pack_start(review, False, False, 0)
        self.callout(f"Installing erases {model.disk_model(device) or 'the disk'} ({device}): everything on it is lost.")
        install = self.button("Erase and install", self.install, "primary")
        self.actions(install, back=self.show_account)
        install.grab_focus()

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
                    self.show_installed, lambda message: self.show_failure("install", message), step="install")

    def show_installed(self):
        self.clear("installed", "OpenShell Native is installed",
                   "Remove the USB stick, then restart: the computer starts from its disk, on your account's lock screen.",
                   step=FINISHED, eyebrow="DONE")
        self.seal("✓", "good")
        restart = self.button("Restart", lambda: self.power("reboot"), "primary")
        self.actions(self.button("Shut down", lambda: self.power("poweroff")), restart)
        restart.grab_focus()

    # ── Updating ─────────────────────────────────────────────────────────

    def prepare_update(self):
        self.release = model.medium_release()
        if not model.medium_signed():
            self.clear("unsigned", "This USB stick cannot update",
                       "It was made without OpenShell Native's release key, so an installed system cannot check what it would get. "
                       "Make the stick again with the release key, or install anew.", step="unsigned")
            self.seal("!", "bad")
            self.actions(self.button("Try OpenShell Native", self.try_live, "primary"))
            return
        self.scan()

    def scan(self):
        self.clear("scan", "Finding installed systems", "Disks are looked at without changing them.", step="selection")
        spinner = Gtk.Spinner()
        spinner.start()
        spinner.set_halign(Gtk.Align.START)
        self.body.pack_start(spinner, False, False, 0)
        self.body.show_all()
        self.set_busy(True)

        def listed(answer):
            self.set_busy(False)
            self.show_selection(model.parse_systems(answer))
        self.in_background(lambda: self.ask("usb-update-list"), listed)

    def show_selection(self, systems):
        self.clear("selection", "Choose your installation",
                   "Update OpenShell Native from this USB stick, or put back the release from before the last update.", step="selection")
        self.callout("This USB stick has OpenShell Native " + (self.release or "?") + ".", "info")
        listbox, _ = self.choice_list("cards")
        for system in systems:
            detail = f"{model.disk_model(system['device']) or 'Disk'}  ·  {model.size_text(system['bytes'])}  ·  {system['device']}"
            if system["previous"]:
                detail += f"  ·  before the last update: {system['previous']}"
            listbox.add(self.choice_row(system, "OpenShell Native " + system["release"], detail, model.disk_kind(system["device"])))
        self.body.pack_start(listbox, False, False, 0)
        if not systems:
            self.callout("No OpenShell Native installation was found on this computer's disks.", "info")
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
            first.grab_focus()

    def show_review(self, restoring=False):
        target = self.target
        if restoring:
            self.clear("review", f"Put back OpenShell Native {target['previous']}?",
                       "The release from before the last update starts again; if it does not, the present one comes back.", step="review")
        else:
            self.clear("review", "Ready to update", "Check the installation before continuing. Your files and settings are kept.", step="review")
        review = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        rows = [("Installed", f"OpenShell Native {target['release']}  ·  "
                              f"{model.disk_model(target['device']) or 'Disk'}  ·  {model.size_text(target['bytes'])}  ·  {target['device']}")]
        if not restoring:
            rows.append(("From the stick", "OpenShell Native " + self.release))
        for caption, value in rows:
            row = styled(Gtk.Box(spacing=16), "review-row")
            title = word(caption.upper(), "caption")
            title.set_size_request(140, -1)
            row.pack_start(title, False, False, 0)
            row.pack_start(word(value, ellipsize=True), True, True, 0)
            review.pack_start(row, False, False, 0)
        self.body.pack_start(review, False, False, 0)
        if not restoring:
            order = model.compare_releases(self.release, target["release"])
            if order == 0:
                self.callout("This USB stick has the release that is installed: continuing installs it again.", "info")
            elif order < 0:
                self.callout("This USB stick has an older release than the installed one: continuing goes back to it.")
        self.body.pack_start(label("The new release starts at the next start of the computer. If it does not start, "
                                   "the computer goes back to the one it has now. Partitions are not formatted.", "muted", "small"), False, False, 0)
        buttons = []
        if restoring:
            buttons.append(self.button("Put it back", lambda: self.update(True), "primary"))
        else:
            if target["previous"]:
                buttons.append(self.button("Put back the previous release…", lambda: self.show_review(True)))
            buttons.append(self.button("Update now", lambda: self.update(False), "primary"))
        self.actions(*buttons, back=self.scan)

    def update(self, restoring):
        device = self.target["device"]
        answer = self.ask(f"usb-update-restore {device}" if restoring else f"usb-update {device}")
        if answer != "started":
            self.show_failure("update", answer)
            return
        title = "Putting back the previous release" if restoring else "Updating OpenShell Native"
        self.follow("usb-update-status", title, "Keep the USB stick connected. Do not turn the computer off.",
                    lambda: self.show_updated(restoring), lambda message: self.show_failure("update", message), step="update")

    def show_updated(self, restoring):
        release = self.target["previous"] if restoring else self.release
        self.clear("updated", "OpenShell Native is ready",
                   f"Remove the USB stick, then restart: OpenShell Native {release} starts, and is kept once it has started.",
                   step=FINISHED, eyebrow="DONE")
        self.seal("✓", "good")
        restart = self.button("Restart", lambda: self.power("reboot"), "primary")
        self.actions(self.button("Shut down", lambda: self.power("poweroff")), restart)
        restart.grab_focus()

    # ── Both ─────────────────────────────────────────────────────────────

    def seal(self, mark, kind):
        """A round mark over the card's text: done, or stopped."""
        seal = word(mark, "seal", kind)
        seal.set_xalign(0.5)
        seal.set_halign(Gtk.Align.START)
        self.seal_slot.pack_start(seal, False, False, 0)
        self.seal_slot.set_margin_bottom(22)
        self.seal_slot.show_all()

    def follow(self, status_request, title, description, success, failure, step):
        """Shows a job's progress until it ends."""
        self.clear("progress", title, description, step=step)
        percent = word("0%", "percent")
        bar = Gtk.ProgressBar()
        line = Gtk.Box(spacing=10)
        spinner = Gtk.Spinner()
        spinner.start()
        message = word("Starting…", "muted", ellipsize=True)
        line.pack_start(spinner, False, False, 0)
        line.pack_start(message, True, True, 0)
        self.body.pack_start(percent, False, False, 0)
        self.body.pack_start(bar, False, False, 0)
        self.body.pack_start(line, False, False, 0)
        self.body.show_all()
        self.set_busy(True)

        def poll():
            state, done, text = model.parse_status(self.ask(status_request))
            if state == "running":
                bar.set_fraction(done / 100)
                percent.set_text(f"{done}%")
                message.set_text(text or "Working…")
                return True
            self.set_busy(False)
            self.poll_source = None
            if state == "done":
                success()
            else:
                failure(text)
            return False
        self.poll_source = GLib.timeout_add(POLL_MS, poll)

    def show_failure(self, what, message):
        step = "install" if what == "install" else "update"
        if what == "install":
            self.clear("failure", "The installation stopped",
                       "The disk may be partly written: installing again starts it over. Read why below.", step=step, eyebrow="STOPPED")
        else:
            self.clear("failure", "The update stopped",
                       "The installed system keeps starting the release it has. Read why below.", step=step, eyebrow="STOPPED")
        self.seal("!", "bad")
        self.callout(message.removeprefix("failed ").strip() or "It stopped.")
        again = self.show_summary if what == "install" and self.disk else (self.scan if what == "update" else self.show_welcome)
        self.actions(self.button("Try again", again, "primary"))

    def request_shutdown(self):
        if self.busy:
            return
        back = self.page
        self.clear("shutdown", "Shut down the computer?", "Remove the USB stick only after the computer has switched off.")
        resume = {"welcome": self.show_welcome, "location": self.show_location, "keyboard": self.show_keyboard,
                  "disk": self.show_disks, "account": self.show_account,
                  "summary": self.show_summary}.get(back, self.show_welcome if self.mode == "install" else self.scan)
        self.actions(self.button("Shut down", lambda: self.power("poweroff"), "primary"), back=resume)

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
