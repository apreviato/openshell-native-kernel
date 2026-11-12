import ctypes
import json
import os
import select
import struct
import sys
import tempfile
from pathlib import Path

IN_CREATE = 0x100
IN_MOVED_FROM = 0x40
IN_MOVED_TO = 0x80
EVENT = struct.Struct("=iIII")


def checked(result, operation):
    if result < 0:
        error = ctypes.get_errno()
        raise OSError(error, f"{operation}: {os.strerror(error)}")
    return result


def inspect(directory, result):
    libc = ctypes.CDLL(None, use_errno=True)
    with tempfile.TemporaryDirectory(prefix="openshell-inotify-", dir=directory) as temporary:
        root = Path(temporary)
        fd = checked(libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC), "inotify_init1")
        try:
            mask = IN_CREATE | IN_MOVED_FROM | IN_MOVED_TO
            watch = checked(libc.inotify_add_watch(fd, os.fsencode(root), mask), "inotify_add_watch")
            with select.epoll() as poller:
                poller.register(fd, select.EPOLLIN)
                source = root / "pending"
                source.write_text("openshell-native", encoding="utf-8")
                source.rename(root / "ready")
                result["epoll_ready"] = any(ready == fd and flags & select.EPOLLIN for ready, flags in poller.poll(0.2))
            try:
                data = os.read(fd, 4096)
            except BlockingIOError:
                data = b""
        finally:
            os.close(fd)

    events = []
    offset = 0
    while offset < len(data):
        descriptor, mask, cookie, size = EVENT.unpack_from(data, offset)
        offset += EVENT.size
        if offset + size > len(data):
            raise ValueError("Truncated inotify event name")
        name = os.fsdecode(data[offset:offset + size].split(b"\0", 1)[0])
        offset += size
        events.append({"watch": descriptor, "mask": hex(mask), "cookie": cookie, "name": name})

    result["events"] = events
    expected = {("pending", IN_CREATE), ("pending", IN_MOVED_FROM), ("ready", IN_MOVED_TO)}
    actual = {(event["name"], int(event["mask"], 16)) for event in events if event["watch"] == watch}
    result["missing"] = [{"name": name, "mask": hex(mask)} for name, mask in sorted(expected - actual)]
    return result["epoll_ready"] and not result["missing"]


def main():
    failed = False
    for directory in sys.argv[1:] or [tempfile.gettempdir()]:
        result = {"probe": "inotify", "directory": directory, "status": "failed"}
        try:
            if inspect(directory, result):
                result["status"] = "passed"
        except (OSError, ValueError, struct.error) as error:
            result["error"] = str(error)
        failed |= result["status"] != "passed"
        print(json.dumps(result), flush=True)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
