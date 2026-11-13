#!/usr/bin/python3
"""Native XSecureLock auth UI, embedded in the locker's protected X11 window."""
import ctypes
from datetime import datetime
import os
from pathlib import Path
import pwd
import resource
import signal
import stat
import subprocess
import sys
import time

# Python -I ignores user modules, PYTHONPATH and the current working directory.
sys.path.insert(0, '/usr/local/lib/openshell')
from lock_auth import Authentication, SecretInput
from lock_settings import MAX_PICTURE, battery as read_battery, load as load_settings, picture_path
from lock_view import CURSOR_BOX, POWER_ACTIONS, LockStyle, ScreenState, caret, caret_box, clock_label, cursor, cursor_kind, hit, paint, strings

FRAME_MS = 16          # Pointer polling: the cursor is drawn by this UI.
CONFIRM_SECONDS = 5    # A second click within this restarts or shuts down.
NOTICE_SECONDS = 4
CURSOR_SIZE = 20
CARET_BLINK = 1.2      # GTK's blink cycle; solid while typing and after
CARET_BLINK_STOP = 10  # this many seconds without a key, as in GTK.


def power_actions():
    """What this computer offers, in the order of POWER_ACTIONS."""
    offered = {'reboot', 'shutdown'}
    try:
        if 'mem' in Path('/sys/power/state').read_text().split():
            offered.add('suspend')
    except OSError:
        pass
    return tuple(action for action in POWER_ACTIONS if action in offered)


def system_cursors(display, Gdk):
    """Images of the session's cursor theme (XSecureLock's grab hides the real
    pointer, so it is drawn): kind -> (cairo surface, hotspot x, hotspot y)."""
    found = {}
    for kind, names in (('default', ('default', 'left_ptr')), ('pointer', ('pointer', 'hand2')), ('text', ('text', 'xterm'))):
        for name in names:
            try:
                theme_cursor = Gdk.Cursor.new_from_name(display, name)
                image = theme_cursor.get_surface() if theme_cursor is not None else None
            except (TypeError, ValueError, AttributeError):
                image = None
            if image and image[0] is not None:
                found[kind] = (image[0], image[1], image[2])
                break
    return found


def picture_surface(path, side, limit):
    """A picture file as a cairo surface no larger than side×side, or None
    when it is missing, too large or unreadable (the screen then goes without)."""
    import cairo
    import gi
    gi.require_version('GdkPixbuf', '2.0')
    gi.require_version('Gdk', '3.0')
    from gi.repository import Gdk, GdkPixbuf, GLib
    path = Path(path)
    try:
        if not path.is_file() or path.stat().st_size > limit:
            return None
        pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(path), side, side, True)
        surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, pixbuf.get_width(), pixbuf.get_height())
        ctx = cairo.Context(surface)
        Gdk.cairo_set_source_pixbuf(ctx, pixbuf, 0, 0)
        ctx.paint()
        return surface
    except (OSError, GLib.Error, cairo.Error, MemoryError):
        return None


def account_photo(home):
    """~/.face, the picture chosen in Settings ▸ Account, as a cairo surface (or None)."""
    return picture_surface(Path(home) / '.face', 128, 2 * 1024 * 1024)


def lock_picture(home):
    """The background picture from Settings ▸ Lock screen, sized for a 4K screen at most (or None)."""
    return picture_surface(picture_path(home), 3840, MAX_PICTURE)


def main():
    if len(sys.argv) != 1:
        return 1
    # Never turn a standalone invocation, closed stdin or closed UI into success.
    if Path(f'/proc/{os.getppid()}/exe').readlink() != Path('/usr/bin/xsecurelock'):
        return 1
    if not stat.S_ISFIFO(os.fstat(0).st_mode):
        return 1
    xid = int(os.environ.get('XSCREENSAVER_WINDOW', '0'), 0)
    if xid <= 0:
        return 1
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    libc = ctypes.CDLL(None)
    if libc.prctl(4, 0, 0, 0, 0) != 0:  # PR_SET_DUMPABLE, no password core dumps.
        return 1

    import cairo
    import gi
    gi.require_version('Gtk', '3.0')
    gi.require_version('Gdk', '3.0')
    gi.require_version('GdkX11', '3.0')
    gi.require_version('PangoCairo', '1.0')
    from gi.repository import Gtk, Gdk, GdkX11, GLib

    display = Gdk.Display.get_default()
    if display is None or not isinstance(display, GdkX11.X11Display):
        return 1
    parent = GdkX11.X11Window.foreign_new_for_display(display, xid)
    if parent is None:
        return 1
    screen = display.get_default_screen()
    account = pwd.getpwuid(os.getuid())
    name = account.pw_gecos.split(',')[0].strip() or account.pw_name
    # Display preferences only (Settings ▸ Lock screen); unlocking ignores them.
    settings = load_settings(account.pw_dir)
    style = LockStyle.from_settings(settings, picture=lock_picture(account.pw_dir) if settings['background'] == 'picture' else None)
    state = ScreenState(name=name, language='en', clock24h=settings['clock24h'], show_name=settings['showName'],
                        actions=power_actions() if settings['powerButtons'] else (),
                        photo=account_photo(account.pw_dir), style=style)
    show_battery = settings['showBattery']
    screen_off = settings['screenOffWhenLocked']
    try:
        keyboard = subprocess.run(['/usr/bin/setxkbmap', '-query'], capture_output=True, text=True, timeout=1)
        fields = dict(line.split(':', 1) for line in keyboard.stdout.splitlines() if ':' in line)
        state.layout = fields.get('layout', '').strip().split(',')[0]
    except (OSError, subprocess.TimeoutExpired):
        pass

    auth = Authentication()
    incoming = SecretInput()
    input_buffer = memoryview(incoming.data).cast('B')
    result = 1
    window = Gtk.Window(type=Gtk.WindowType.POPUP)
    window.set_decorated(False)
    window.set_title('OpenShell · Locked')
    window.connect('delete-event', lambda *_: True)
    canvas = Gtk.DrawingArea()
    canvas.get_accessible().set_name(strings(state.language)['locked'])
    window.add(canvas)
    rectangles = []
    scenes = {}
    pointer_at = None       # Root coordinates of the last poll.
    pointer_shown = False   # Shown once the mouse moves, hidden while typing.
    pressed_before = False
    caret_shown = True
    caret_since = time.monotonic()  # Last key: the caret blinks from here.
    last_activity = time.monotonic()
    armed_at = notice_at = battery_at = 0
    power = None

    cursors = system_cursors(display, Gdk)

    def snapshot():
        """Everything the painted scene depends on, apart from the cursor."""
        clock = clock_label(state.now, state.clock24h) + (state.now.date(),)
        spinner = state.now.microsecond // 50_000 if state.busy else 0
        return (state.count, state.busy, state.failed, state.caps, state.prompt, state.layout, state.battery,
                state.hover, state.pressed, state.armed, state.notice, clock, spinner)

    def cursor_area(x, y, kind):
        """The area the pointer image covers, to repaint only that when it moves."""
        if kind in cursors:
            surface, x_hot, y_hot = cursors[kind]
            scale = surface.get_device_scale()[0] if hasattr(surface, 'get_device_scale') else 1
            return int(x - x_hot) - 1, int(y - y_hot) - 1, int(surface.get_width() / scale) + 3, int(surface.get_height() / scale) + 3
        bx, by, bw, bh = CURSOR_BOX
        k = CURSOR_SIZE / 20
        return int(x + bx*k) - 1, int(y + by*k) - 1, int(bw*k) + 3, int(bh*k) + 3

    def caret_areas():
        for rect in rectangles:
            box = caret_box(rect.width, rect.height, state)
            if box:
                yield rect.x + box[0], rect.y + box[1], box[2], box[3]

    def resize(*_):
        nonlocal rectangles
        # GDK sizes are logical pixels; the protected foreign X window uses
        # the same scale, so both monitors and HiDPI use the current geometry.
        _, _, width, height = screen.get_root_window().get_geometry()
        parent.move_resize(0, 0, width, height)
        parent.show()
        window.resize(width, height)
        window.set_size_request(width, height)
        rectangles = [display.get_monitor(i).get_geometry() for i in range(display.get_n_monitors())]
        scenes.clear()
        canvas.queue_draw()

    def draw(area, ctx):
        key = snapshot()
        for index, rect in enumerate(rectangles):
            # The scene is painted once per change and reused while only the
            # pointer moves, so the cursor stays smooth on large screens.
            cached = scenes.get(index)
            if cached is None or cached[0] != key:
                surface = area.get_window().create_similar_surface(cairo.CONTENT_COLOR, rect.width, rect.height)
                paint(cairo.Context(surface), rect.width, rect.height, state)
                cached = scenes[index] = (key, surface)
            ctx.save()
            ctx.rectangle(rect.x, rect.y, rect.width, rect.height)
            ctx.clip()
            ctx.set_source_surface(cached[1], rect.x, rect.y)
            ctx.paint()
            if caret_shown:
                ctx.translate(rect.x, rect.y)
                caret(ctx, rect.width, rect.height, state)
            ctx.restore()
        if pointer_shown and pointer_at:
            kind = cursor_kind(state.hover)
            if kind in cursors:
                # The desktop's own cursor theme: the same arrow, hand and text cursor.
                surface, x_hot, y_hot = cursors[kind]
                ctx.set_source_surface(surface, pointer_at[0] - x_hot, pointer_at[1] - y_hot)
                ctx.paint()
            else:
                cursor(ctx, pointer_at[0], pointer_at[1], CURSOR_SIZE)
        return False

    canvas.connect('draw', draw)
    window.realize()
    window.get_window().set_title('OpenShell · Locked')
    # No independent unlocked fullscreen window: draw as a child of the
    # exact auth window which XSecureLock keeps above the saver and desktop.
    window.get_window().reparent(parent, 0, 0)
    resize()
    screen.connect('size-changed', resize)
    screen.connect('monitors-changed', resize)
    window.show_all()
    os.set_blocking(0, False)
    auth.start()

    def run_power(action):
        nonlocal power
        if power is not None:
            return
        state.armed = None
        # A password typed so far is not kept across sleep or shutdown.
        auth.secret.clear()
        verb = 'poweroff' if action == 'shutdown' else action
        try:
            process = subprocess.Popen(['/usr/bin/systemctl', verb], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL, start_new_session=True)
        except OSError:
            show_notice(action)
            return
        power = (action, process)

    def show_notice(action):
        nonlocal notice_at
        state.notice = f'{strings(state.language)[action]} is not available'
        notice_at = time.monotonic()

    def press(target):
        nonlocal armed_at
        if target == 'submit':
            state.armed = None
            auth.submit()
        elif target == 'suspend':
            run_power(target)
        elif target in ('reboot', 'shutdown'):
            if state.armed == target:
                run_power(target)
            else:
                state.armed, armed_at = target, time.monotonic()
        else:
            state.armed = None

    def pointer_target():
        if not (pointer_shown and pointer_at):
            return None
        x, y = pointer_at
        for rect in rectangles:
            if rect.x <= x < rect.x + rect.width and rect.y <= y < rect.y + rect.height:
                return hit(rect.width, rect.height, state, x - rect.x, y - rect.y)
        return None

    def refresh():
        nonlocal result, pressed_before, pointer_at, pointer_shown, caret_shown, caret_since, last_activity, battery_at, power
        try:
            auth.tick()
            if auth.accepted:
                result = 0  # The only successful exit: the real PAM helper.
                Gtk.main_quit()
                return False
            now = time.monotonic()
            state.now = datetime.now()
            before = snapshot()
            cursor_before = (pointer_shown, pointer_at, cursor_kind(state.hover))
            try:
                got = os.readv(0, [input_buffer])
            except BlockingIOError:
                got = None
            if got == 0:
                Gtk.main_quit()
                return False
            if got:
                try:
                    for i in range(got):
                        auth.key(input_buffer[i])
                finally:
                    incoming.clear()
                pointer_shown = False
                state.armed = None
                last_activity = now
            # XSecureLock retains its pointer grab (with an invisible cursor).
            # Query position/button state without releasing or replacing it.
            pointer = display.get_default_seat().get_pointer()
            _, px, py = pointer.get_position()
            _, _, _, mask = screen.get_root_window().get_device_position(pointer)
            pressed = bool(mask & Gdk.ModifierType.BUTTON1_MASK)
            if pointer_at is not None and (px, py) != pointer_at:
                pointer_shown = True
                last_activity = now
            pointer_at = (px, py)
            target = pointer_target()
            # Like any button: drawn pressed while held, acting when released
            # over the same control (dragging away cancels).
            if pressed and not pressed_before:
                pointer_shown = True
                last_activity = now
                target = pointer_target()
                state.pressed = target
                if target is None:
                    state.armed = None
            elif not pressed and pressed_before:
                if state.pressed and state.pressed == target:
                    press(target)
                state.pressed = None
            pressed_before = pressed
            state.hover = target

            if state.armed and now - armed_at > CONFIRM_SECONDS:
                state.armed = None
            if state.notice and now - notice_at > NOTICE_SECONDS:
                state.notice = ''
            if power is not None and power[1].poll() is not None:
                if power[1].returncode != 0:
                    show_notice(power[0])
                power = None
            if show_battery and now - battery_at > 30:
                state.battery = read_battery()
                battery_at = now

            state.count, state.busy, state.failed = auth.secret.characters, auth.busy, auth.failed
            state.caps = Gdk.Keymap.get_for_display(display).get_caps_lock_state()
            prompt = auth.message.strip()
            # Standard system prompts become product language; special PAM
            # challenges (OTP, expired password) retain their own instructions.
            state.prompt = '' if prompt.lower().rstrip(':') in ('password', 'senha', 'passwort', 'mot de passe', 'contraseña') else prompt[:200]
            # A key (or the end of a check) shows the caret solid, then it blinks.
            if got or state.busy:
                caret_since = now
            caret_before = caret_shown
            blink = now - caret_since
            caret_shown = blink > CARET_BLINK_STOP or blink % CARET_BLINK < CARET_BLINK / 2

            if snapshot() != before:
                canvas.queue_draw()
            else:
                if (pointer_shown, pointer_at, cursor_kind(state.hover)) != cursor_before:
                    for shown, at, kind in (cursor_before, (pointer_shown, pointer_at, cursor_kind(state.hover))):
                        if shown and at:
                            canvas.queue_draw_area(*cursor_area(*at, kind))
                if caret_shown != caret_before:
                    for area in caret_areas():
                        canvas.queue_draw_area(*area)
            # Idle on the lock screen: close, so XSecureLock turns the panel off.
            # Any key or mouse movement brings this screen back.
            idle = now - max(last_activity, auth.last_input)
            if screen_off and idle > screen_off and power is None:
                Gtk.main_quit()
                return False
            return True
        except Exception:
            # Avoid logging data from input or PAM conversations.
            print('OpenShell lock UI failed; session remains locked.', file=sys.stderr)
            Gtk.main_quit()
            return False

    def interrupted(*_):
        Gtk.main_quit()
        return False

    GLib.timeout_add(FRAME_MS, refresh)
    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, interrupted)
    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGINT, interrupted)
    try:
        Gtk.main()
    finally:
        incoming.close()
        auth.close()
        window.destroy()
    return result


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception:
        print('OpenShell lock UI unavailable; session remains locked.', file=sys.stderr)
        sys.exit(1)
