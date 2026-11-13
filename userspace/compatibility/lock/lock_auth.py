"""Native password input and the Debian XSecureLock PAM conversation.

XSecureLock owns the display/input grabs and forwards UTF-8 keys on stdin.
The signed authproto_pam executable alone decides whether to unlock. Protocol:
https://github.com/google/xsecurelock/blob/master/helpers/authproto.h
"""
import ctypes
import os
import re
import subprocess
import time

PAM_HELPER = '/usr/libexec/xsecurelock/authproto_pam'


class SecretInput:
    """Fixed mutable, pinned storage. Never make a Python password string."""
    def __init__(self, require_locked=True):
        self.data = (ctypes.c_ubyte * 1024)()
        self.length = 0
        self.libc = ctypes.CDLL(None if os.name == 'posix' else 'msvcrt', use_errno=True)
        self.locked = hasattr(self.libc, 'mlock') and self.libc.mlock(ctypes.byref(self.data), ctypes.c_size_t(len(self.data))) == 0
        if require_locked and not self.locked:
            raise RuntimeError('Unable to protect password memory')

    def clear(self):
        ctypes.memset(ctypes.byref(self.data), 0, len(self.data))
        self.length = 0

    def append(self, byte):
        if self.length >= len(self.data):
            self.clear()
            raise ValueError('Input too long')
        self.data[self.length] = byte
        self.length += 1

    def backspace(self):
        if not self.length:
            return
        at = self.length - 1
        while at > 0 and self.data[at] & 0xc0 == 0x80:
            at -= 1
        ctypes.memset(ctypes.byref(self.data, at), 0, self.length - at)
        self.length = at

    @property
    def characters(self):
        return sum(self.data[i] & 0xc0 != 0x80 for i in range(self.length))

    def close(self):
        self.clear()
        if self.locked:
            self.libc.munlock(ctypes.byref(self.data), ctypes.c_size_t(len(self.data)))
            self.locked = False


class PacketReader:
    """Bounded incremental decoder; partial reads are normal, bad frames fail."""
    def __init__(self):
        self.buffer = bytearray()

    def feed(self, data):
        self.buffer.extend(data)
        while self.buffer:
            end = self.buffer.find(b'\n')
            if end < 0:
                if len(self.buffer) > 12:
                    raise ValueError('Invalid PAM packet header')
                return
            header = self.buffer[:end]
            if not re.fullmatch(rb'[iPeU] [0-9]{1,5}', header):
                raise ValueError('Invalid PAM packet header')
            size = int(header[2:])
            if size >= 65535:
                raise ValueError('Oversized PAM message')
            stop = end + 1 + size
            if len(self.buffer) <= stop:
                return
            if self.buffer[stop] != 10:
                raise ValueError('Invalid PAM packet terminator')
            kind = chr(header[0])
            message = bytes(self.buffer[end + 1:stop]).decode('utf-8', errors='replace')
            del self.buffer[:stop + 1]
            yield kind, message


def write_all(fd, data):
    view = memoryview(data).cast('B')
    while view:
        count = os.write(fd, view)
        if count <= 0:
            raise OSError('PAM pipe closed')
        view = view[count:]


class Authentication:
    def __init__(self, helper=PAM_HELPER, require_locked=True):
        self.helper = helper
        self.secret = SecretInput(require_locked)
        self.reader = PacketReader()
        self.process = None
        self.prompt = None
        self.message = ''
        self.failed = False
        self.busy = True
        self.accepted = False
        self.closed = False
        self.retry_at = 0
        self.last_input = time.monotonic()
        self.eof = False
        self.previous_prompt = None

    def start(self):
        self.secret.clear()
        self.reader = PacketReader()
        self.prompt = None
        self.busy = True
        self.eof = False
        self.previous_prompt = None
        # No shell, credentials in arguments, or user-selected PAM service.
        env = {**os.environ, 'XSECURELOCK_PAM_SERVICE': 'openshell-lock', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8', 'LANGUAGE': 'en'}
        self.process = subprocess.Popen([self.helper], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, bufsize=0)
        os.set_blocking(self.process.stdout.fileno(), False)

    def key(self, byte):
        self.last_input = time.monotonic()
        if byte in (1, 21, 27):  # Ctrl+A / Ctrl+U / Esc clear, never unlock.
            self.secret.clear()
            return
        if self.prompt is None or self.busy:
            return
        if byte in (8, 127):
            self.secret.backspace()
        elif byte in (10, 13):
            self.submit()
        elif byte >= 32:
            self.secret.append(byte)
            self.failed = False

    def submit(self):
        if self.prompt is None or self.busy or not self.secret.length:
            return
        fd = self.process.stdin.fileno()
        try:
            write_all(fd, f'{self.prompt.lower()} {self.secret.length}\n'.encode('ascii'))
            write_all(fd, memoryview(self.secret.data)[:self.secret.length])
            write_all(fd, b'\n')
        finally:
            self.secret.clear()
        self.prompt = None
        self.busy = True
        self.previous_prompt = self.message

    def tick(self):
        if self.closed or self.accepted:
            return
        if self.process is None:
            if time.monotonic() >= self.retry_at:
                self.start()
            return
        while not self.eof:
            try:
                data = os.read(self.process.stdout.fileno(), 8192)
            except BlockingIOError:
                break
            if not data:
                self.eof = True
                break
            for kind, message in self.reader.feed(data):
                if kind in ('P', 'U'):
                    self.secret.clear()
                    if self.previous_prompt == message:
                        self.failed = True
                    self.prompt = kind
                    self.message = message
                    self.busy = False
                elif kind == 'e':
                    self.failed = True
                # PAM info is advisory; it must never signal success.
        status = self.process.poll()
        if status is not None and self.eof:
            if self.reader.buffer:
                raise ValueError('Truncated PAM message')
            self.secret.clear()
            self.process.stdin.close()
            self.process.stdout.close()
            self.process = None
            if status == 0:
                self.accepted = True
            else:
                self.failed = True
                self.prompt = None
                self.busy = True
                self.retry_at = time.monotonic() + 1.5

    def close(self):
        self.closed = True
        if self.process is not None:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
            self.process.stdin.close()
            self.process.stdout.close()
            self.process = None
        self.secret.close()
