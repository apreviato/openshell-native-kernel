#!/usr/bin/python3
"""Remember the X session's Num Lock before the first password prompt.

Observe XKB modifier events, including while XSecureLock owns the keyboard.
No key grab, password input or periodic subprocess is needed. The session,
not Electron/hostd, owns this process and the per-user state file.
"""
import argparse
import ctypes as C
import fcntl
import os
from pathlib import Path
import select
import signal
import tempfile

CORE_KEYBOARD = 0x100
NUM_LOCK = 0xff7f
MODIFIER_LOCK = 1 << 3


class State(C.Structure):
    _fields_ = [('group', C.c_ubyte), ('locked_group', C.c_ubyte), ('base_group', C.c_ushort),
                ('latched_group', C.c_ushort), *[(name, C.c_ubyte) for name in
                 ('mods', 'base_mods', 'latched_mods', 'locked_mods', 'compat_state', 'grab_mods',
                  'compat_grab_mods', 'lookup_mods', 'compat_lookup_mods')], ('ptr_buttons', C.c_ushort)]


class AnyXkbEvent(C.Structure):
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int),
                ('display', C.c_void_p), ('time', C.c_ulong), ('xkb_type', C.c_int), ('device', C.c_int)]


class StateEvent(C.Structure):
    _fields_ = AnyXkbEvent._fields_ + [('changed', C.c_uint),
        *[(name, C.c_int) for name in ('group', 'base_group', 'latched_group', 'locked_group')],
        *[(name, C.c_uint) for name in ('mods', 'base_mods', 'latched_mods', 'locked_mods')],
        ('compat_state', C.c_int), *[(name, C.c_ubyte) for name in ('grab_mods', 'compat_grab_mods', 'lookup_mods', 'compat_lookup_mods')],
        ('ptr_buttons', C.c_int), *[(name, C.c_ubyte) for name in ('keycode', 'event_type', 'req_major', 'req_minor')]]


class Event(C.Union):
    _fields_ = [('xkb', AnyXkbEvent), ('state', StateEvent), ('pad', C.c_long * 24)]


def save(path, enabled):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        temp = Path(stream.name)
        try:
            stream.write('on\n' if enabled else 'off\n')
            stream.flush()
            os.fsync(stream.fileno())
            os.replace(temp, path)
            descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        finally:
            temp.unlink(missing_ok=True)


def load(path):
    try:
        value = path.read_text().strip()
        if value in ('on', 'off'):
            return value == 'on'
    except (OSError, UnicodeError):
        pass
    return None


class Keyboard:
    def __init__(self):
        self.x = C.CDLL('libX11.so.6')
        for name, args, result in (
            ('XOpenDisplay', [C.c_char_p], C.c_void_p),
            ('XCloseDisplay', [C.c_void_p], C.c_int),
            ('XConnectionNumber', [C.c_void_p], C.c_int),
            ('XPending', [C.c_void_p], C.c_int),
            ('XNextEvent', [C.c_void_p, C.POINTER(Event)], C.c_int),
            ('XSync', [C.c_void_p, C.c_int], C.c_int),
            ('XkbQueryExtension', [C.c_void_p, *([C.POINTER(C.c_int)] * 5)], C.c_int),
            ('XkbSelectEvents', [C.c_void_p, C.c_uint, C.c_uint, C.c_uint], C.c_int),
            ('XkbSelectEventDetails', [C.c_void_p, C.c_uint, C.c_uint, C.c_ulong, C.c_ulong], C.c_int),
            ('XkbKeysymToModifiers', [C.c_void_p, C.c_ulong], C.c_uint),
            ('XkbGetState', [C.c_void_p, C.c_uint, C.POINTER(State)], C.c_int),
            ('XkbLockModifiers', [C.c_void_p, C.c_uint, C.c_uint, C.c_uint], C.c_int),
            ('XkbRefreshKeyboardMapping', [C.c_void_p], C.c_int),
        ):
            function = getattr(self.x, name)
            function.argtypes, function.restype = args, result
        self.display = self.x.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError('Num Lock: no X11 display')
        opcode, base, error, major, minor = [C.c_int(value) for value in (0, 0, 0, 1, 0)]
        if not self.x.XkbQueryExtension(self.display, *[C.byref(value) for value in (opcode, base, error, major, minor)]):
            raise RuntimeError('Num Lock: XKB is unavailable')
        self.base = base.value
        # NewKeyboard, Map and State; only locked-modifier changes in State.
        if not self.x.XkbSelectEvents(self.display, CORE_KEYBOARD, 7, 7):
            raise RuntimeError('Num Lock: could not observe keyboard state')
        if not self.x.XkbSelectEventDetails(self.display, CORE_KEYBOARD, 2, 0x3fff, MODIFIER_LOCK):
            raise RuntimeError('Num Lock: could not observe modifier locks')
        self.x.XSync(self.display, False)

    def current(self):
        mask = self.x.XkbKeysymToModifiers(self.display, NUM_LOCK)
        state = State()
        if not mask or self.x.XkbGetState(self.display, CORE_KEYBOARD, C.byref(state)) != 0:
            return None
        return bool(state.locked_mods & mask)

    def restore(self, enabled):
        mask = self.x.XkbKeysymToModifiers(self.display, NUM_LOCK)
        if mask:
            if not self.x.XkbLockModifiers(self.display, CORE_KEYBOARD, mask, mask if enabled else 0):
                raise RuntimeError('Num Lock: could not restore keyboard state')
            self.x.XSync(self.display, False)

    def changes(self):
        remapped, requested = False, None
        while self.x.XPending(self.display):
            event = Event()
            self.x.XNextEvent(self.display, C.byref(event))
            if event.xkb.type != self.base:
                continue
            if event.xkb.xkb_type == 1:
                self.x.XkbRefreshKeyboardMapping(C.byref(event))
            remapped |= event.xkb.xkb_type in (0, 1)
            # A key or an explicit XkbLatchLockState request (opcode 5), not
            # the incidental clearing of modifiers while a keymap is replaced.
            if event.xkb.xkb_type == 2 and event.state.changed & MODIFIER_LOCK and (event.state.keycode or event.state.req_minor == 5):
                mask = self.x.XkbKeysymToModifiers(self.display, NUM_LOCK)
                if mask:
                    requested = bool(event.state.locked_mods & mask)
        return remapped, requested

    def close(self):
        self.x.XCloseDisplay(self.display)


def watch(path, ready):
    keyboard = Keyboard()
    desired = load(path)
    running = True
    def stop(*_):
        nonlocal running
        running = False
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        if desired is not None:
            keyboard.restore(desired)
        else:
            desired = keyboard.current()
        if desired is not None:
            save(path, desired)
        ready.write_text(str(os.getpid()))
        while running:
            if not keyboard.x.XPending(keyboard.display):
                select.select([keyboard.x.XConnectionNumber(keyboard.display)], [], [], .2)
            remapped, requested = keyboard.changes()
            if requested is not None and requested != desired:
                save(path, requested)
                desired = requested
            if remapped:
                if desired is not None:
                    keyboard.restore(desired)
                else:
                    desired = keyboard.current()
                    if desired is not None:
                        save(path, desired)
        # Capture a key just before a normal reboot/shutdown begins too.
        value = keyboard.current()
        if value is not None and value != desired:
            save(path, value)
    finally:
        ready.unlink(missing_ok=True)
        keyboard.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state', type=Path, required=True)
    parser.add_argument('--ready', type=Path, required=True)
    args = parser.parse_args()
    args.ready.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with args.ready.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        watch(args.state, args.ready)
