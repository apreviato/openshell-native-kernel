"""Namespace operations that the Chromium sandbox uses before it installs seccomp filters.

An unprivileged process enters new user, PID and network namespaces, maps its
identity, removes its filesystem view and drops its capabilities. Each step
reports one JSON line.
"""

import ctypes
import errno
import json
import os
import signal
import socket
import sys

UNPRIVILEGED = 65534
ABSTRACT_NAME = b"\x00openshell-native-namespaces"
LINUX_CAPABILITY_VERSION_3 = 0x20080522
CAP_SYS_ADMIN = 21
CAP_SYS_CHROOT = 18
PR_SET_DUMPABLE = 4
SYS_CLONE = 56
CLONE_FS = 0x200


class CapabilityHeader(ctypes.Structure):
    _fields_ = [("version", ctypes.c_uint32), ("pid", ctypes.c_int)]


class CapabilityData(ctypes.Structure):
    _fields_ = [("effective", ctypes.c_uint32), ("permitted", ctypes.c_uint32), ("inheritable", ctypes.c_uint32)]


libc = ctypes.CDLL(None, use_errno=True)


def capabilities(new=None):
    header = CapabilityHeader(LINUX_CAPABILITY_VERSION_3, 0)
    data = (CapabilityData * 2)()
    if new is not None:
        for index in range(2):
            data[index].effective = data[index].permitted = (new >> (32 * index)) & 0xFFFFFFFF
        call = libc.capset
    else:
        call = libc.capget
    if call(ctypes.byref(header), data) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))
    return data[0].effective | (data[1].effective << 32)


def write(path, value):
    descriptor = os.open(path, os.O_WRONLY)
    try:
        os.write(descriptor, value.encode())
    finally:
        os.close(descriptor)


def report(name, status, **details):
    print(json.dumps({"probe": f"namespaces_{name}", "status": status, **details}), flush=True)


def step(name, operation):
    try:
        operation()
    except Exception as error:
        report(name, "failed", error=str(error))
        raise
    report(name, "passed")


def become_unprivileged():
    if os.geteuid() == 0:
        os.setgroups([])
        os.setresgid(UNPRIVILEGED, UNPRIVILEGED, UNPRIVILEGED)
        os.setresuid(UNPRIVILEGED, UNPRIVILEGED, UNPRIVILEGED)
        # Changing the identity hides /proc/self from its own process until the
        # process is marked dumpable again.
        if libc.prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "PR_SET_DUMPABLE")
    if capabilities() != 0:
        raise RuntimeError("An unprivileged process kept capabilities")


def enter_namespaces():
    uid, gid = os.getuid(), os.getgid()
    os.unshare(os.CLONE_NEWUSER | os.CLONE_NEWPID | os.CLONE_NEWNET)
    write("/proc/self/setgroups", "deny")
    write("/proc/self/uid_map", f"{uid} {uid} 1")
    write("/proc/self/gid_map", f"{gid} {gid} 1")
    if (os.getuid(), os.getgid()) != (uid, gid):
        raise RuntimeError("The identity mapping changed the visible identity")
    owned = capabilities()
    if not owned & (1 << CAP_SYS_ADMIN) or not owned & (1 << CAP_SYS_CHROOT):
        raise RuntimeError("The creator of a user namespace did not receive its capabilities")


def isolated_network():
    names = {name for _, name in socket.if_nameindex()}
    if names - {"lo"}:
        raise RuntimeError(f"The new network namespace exposes {sorted(names)}")


def drop_filesystem():
    # A helper that shares the filesystem state moves the root into a directory
    # that disappears with the helper. The caller is left with an empty root.
    child = libc.syscall(SYS_CLONE, CLONE_FS | signal.SIGCHLD, 0, 0, 0, 0)
    if child == -1:
        error = ctypes.get_errno()
        raise OSError(error, f"clone: {os.strerror(error)}")
    if child == 0:
        try:
            os.chroot("/proc/self/fdinfo/")
            os.chdir("/")
        except BaseException:
            os._exit(1)
        os._exit(0)
    _, status = os.waitpid(child, 0)
    if status != 0:
        raise RuntimeError("The helper could not replace the root directory")
    gone = (errno.ENOENT, errno.ESRCH, errno.ENOTDIR)
    try:
        entries = os.listdir("/")
    except OSError as error:
        if error.errno not in gone:
            raise
        entries = []
    if entries:
        raise RuntimeError(f"The root directory still lists {entries[:4]}")
    for path in ("/etc", "/proc/self/status"):
        try:
            os.stat(path)
        except OSError as error:
            if error.errno in gone:
                continue
            raise
        raise RuntimeError(f"{path} is still reachable")


def drop_capabilities():
    capabilities(0)
    if capabilities() != 0:
        raise RuntimeError("Capabilities remained after dropping them")
    try:
        os.unshare(os.CLONE_NEWNET)
    except PermissionError:
        return
    raise RuntimeError("A namespace was created without CAP_SYS_ADMIN")


class Outside:
    """Resources that the probe creates before it enters the namespaces."""

    def __init__(self):
        self.secret = None
        self.listener = None
        self.sleeper = None

    def prepare(self):
        # A file that only the initial root user may read.
        if os.geteuid() == 0:
            self.secret = f"/tmp/openshell-native-secret-{os.getpid()}"
            descriptor = os.open(self.secret, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
            os.write(descriptor, b"secret")
            os.close(descriptor)
        # An abstract socket in the initial network namespace.
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(ABSTRACT_NAME)
        self.listener.listen(1)
        # A process in the initial PID namespace.
        self.sleeper = os.fork()
        if self.sleeper == 0:
            signal.pause()
            os._exit(0)

    def cleanup(self):
        if self.sleeper:
            os.kill(self.sleeper, signal.SIGKILL)
            os.waitpid(self.sleeper, 0)
        if self.listener:
            self.listener.close()
        if self.secret:
            os.unlink(self.secret)


OUTSIDE = Outside()


def expect_error(operation, errors, description):
    try:
        operation()
    except OSError as error:
        if error.errno in errors:
            return
        raise RuntimeError(f"{description}: unexpected {errno.errorcode.get(error.errno, error.errno)}")
    raise RuntimeError(f"{description} succeeded")


def identity_is_confined():
    # The capabilities of the namespace do not reach IDs that it does not map.
    expect_error(lambda: os.setuid(0), (errno.EINVAL, errno.EPERM), "setuid to the unmapped root user")
    expect_error(lambda: write("/proc/self/uid_map", "0 0 1"), (errno.EPERM, errno.EACCES),
                 "rewriting the UID map")
    if OUTSIDE.secret:
        expect_error(lambda: os.open(OUTSIDE.secret, os.O_RDONLY), (errno.EACCES, errno.EPERM),
                     "reading a file of the initial root user")


def outside_is_hidden():
    # Local IDs 1 and 2 belong to this namespace, so only a larger global ID is
    # unambiguous.
    if OUTSIDE.sleeper > 2:
        expect_error(lambda: os.kill(OUTSIDE.sleeper, 0), (errno.ESRCH,),
                     "signaling a process outside the PID namespace")
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        expect_error(lambda: client.connect(ABSTRACT_NAME), (errno.ECONNREFUSED, errno.ENOENT),
                     "connecting to an abstract socket of the initial network namespace")
    finally:
        client.close()


def init_of_namespace():
    if os.getpid() != 1:
        raise RuntimeError(f"Expected PID 1 in the new PID namespace, got {os.getpid()}")
    if os.getppid() != 0:
        raise RuntimeError("The parent outside the PID namespace is visible")


def sandboxed_child():
    try:
        step("pid", init_of_namespace)
        step("network", isolated_network)
        step("identity", identity_is_confined)
        step("outside", outside_is_hidden)
        step("filesystem", drop_filesystem)
        step("capabilities", drop_capabilities)
    except Exception:
        os._exit(1)
    os._exit(0)


def main():
    OUTSIDE.prepare()
    try:
        # The namespaces are entered in a child, so that the cleanup runs with the
        # identity and the namespaces of the start.
        prober = os.fork()
        if prober == 0:
            try:
                step("unprivileged", become_unprivileged)
                step("enter", enter_namespaces)
            except Exception:
                os._exit(1)
            # The first child of the process becomes the init of the new PID namespace.
            child = os.fork()
            if child == 0:
                sandboxed_child()
            _, status = os.waitpid(child, 0)
            os._exit(0 if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0 else 1)
        _, status = os.waitpid(prober, 0)
    finally:
        OUTSIDE.cleanup()
    if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
        return 1
    print("OPENSHELL_NATIVE_NAMESPACES_OK", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
