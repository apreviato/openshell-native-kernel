import array
import fcntl
import json
import mmap
import os
import pty
import selectors
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

FIXTURE = b"openshell-native"
TIMEOUT = 8


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def processes():
    result = subprocess.run(
        [sys.executable, "-I", "-c", "import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())"],
        input=FIXTURE, capture_output=True, timeout=TIMEOUT, check=True,
    )
    require(result.stdout == FIXTURE, "Subprocess pipe returned unexpected data")

    with subprocess.Popen(
        [sys.executable, "-I", "-c", "import signal; signal.pause()"],
    ) as child:
        try:
            child.terminate()
            require(child.wait(timeout=TIMEOUT) == -signal.SIGTERM, "Unexpected subprocess exit status")
        finally:
            if child.poll() is None:
                child.kill()


def terminal():
    master, slave = pty.openpty()
    try:
        require(os.isatty(slave), "PTY slave did not report itself as a terminal")
        size = struct.pack("HHHH", 29, 91, 0, 0)
        fcntl.ioctl(master, termios.TIOCSWINSZ, size)
        require(fcntl.ioctl(slave, termios.TIOCGWINSZ, bytes(len(size))) == size, "PTY resize failed")
    finally:
        os.close(slave)
        os.close(master)


def mapped_memory():
    descriptor = os.memfd_create("openshell-compat", os.MFD_CLOEXEC)
    try:
        os.ftruncate(descriptor, mmap.PAGESIZE)
        with mmap.mmap(descriptor, mmap.PAGESIZE) as mapping:
            mapping[:len(FIXTURE)] = FIXTURE
            require(os.pread(descriptor, len(FIXTURE), 0) == FIXTURE, "Shared mapping did not reach memfd")
    finally:
        os.close(descriptor)


def descriptor_passing():
    sender, receiver = socket.socketpair()
    with sender, receiver, tempfile.TemporaryFile() as fixture, selectors.DefaultSelector() as selector:
        fixture.write(FIXTURE)
        fixture.flush()
        fixture.seek(0)
        descriptors = array.array("i", [fixture.fileno()])
        selector.register(receiver, selectors.EVENT_READ)
        sender.sendmsg([b"!"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, descriptors)])
        require(selector.select(TIMEOUT), "Descriptor message timed out")
        data, ancillary, flags, _ = receiver.recvmsg(1, socket.CMSG_SPACE(descriptors.itemsize))
        received = array.array("i")
        for level, kind, content in ancillary:
            if (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS):
                received.frombytes(content[:len(content) - len(content) % received.itemsize])
        try:
            require(data == b"!" and not flags & socket.MSG_CTRUNC and len(received) == 1, "Invalid descriptor message")
            require(os.read(received[0], len(FIXTURE)) == FIXTURE, "Received descriptor returned unexpected data")
        finally:
            for descriptor in received:
                os.close(descriptor)


def filesystem():
    with tempfile.TemporaryDirectory(prefix="openshell-python-", dir=os.environ.get("OPENSHELL_PROBE_TMP")) as directory:
        path = Path(directory)
        source, target = path / "pending", path / "ready"
        with source.open("xb") as stream:
            stream.write(FIXTURE)
            stream.flush()
            os.fsync(stream.fileno())
        source.replace(target)
        descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        require(target.read_bytes() == FIXTURE, "Atomic rename returned unexpected data")


def loopback_udp():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
        receiver.settimeout(TIMEOUT)
        sender.settimeout(TIMEOUT)
        receiver.bind(("127.0.0.1", 0))
        sender.sendto(FIXTURE, receiver.getsockname())
        require(receiver.recv(len(FIXTURE)) == FIXTURE, "UDP loopback returned unexpected data")


def server_restart():
    # A restarted server binds its port again while the connections of the
    # previous one are still closing, as hostd does after a crash.
    first = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    first.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    first.bind(("127.0.0.1", 0))
    first.listen()
    address = first.getsockname()
    client = socket.create_connection(address, timeout=TIMEOUT)
    accepted, _ = first.accept()
    # The server closes first, so its side of the connection lingers.
    accepted.close()
    first.close()
    second = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        second.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        second.bind(address)
        second.listen()
        with socket.create_connection(address, timeout=TIMEOUT) as again:
            served, _ = second.accept()
            with served:
                again.sendall(FIXTURE)
                require(served.recv(len(FIXTURE)) == FIXTURE, "The restarted server got unexpected data")
    finally:
        second.close()
        client.close()


def clock_and_entropy():
    before = time.monotonic_ns()
    require(len(os.urandom(32)) == 32, "Entropy unavailable")
    require(time.monotonic_ns() >= before, "Monotonic clock went backwards")


print(json.dumps({"runtime": "python", "version": sys.version.split()[0], "platform": sys.platform}), flush=True)
failed = False
for probe in (processes, terminal, mapped_memory, descriptor_passing, filesystem, loopback_udp, server_restart, clock_and_entropy):
    try:
        probe()
        print(json.dumps({"probe": probe.__name__, "status": "passed"}), flush=True)
    except Exception as error:
        failed = True
        print(json.dumps({"probe": probe.__name__, "status": "failed", "error": str(error)}), flush=True)
if not failed:
    print("OPENSHELL_NATIVE_PYTHON_OK", flush=True)
sys.exit(int(failed))
