"""What the live medium's setup screen (openshell_setup.py) knows, without
its window: the session supervisor's answers (userspace/shell: install.rs,
usb_update.rs, power.rs), the time zones and keyboard layouts the image has,
and how two releases compare. Python's standard library only."""
import os
import re
import socket
from pathlib import Path

SYSTEM_SOCKET_ENV = "OPENSHELL_SYSTEM_SOCKET"
# The supervisor reads one line per connection, at most this long.
MAX_REQUEST = 288
ZONEINFO = Path(os.environ.get("OPENSHELL_ZONEINFO", "/usr/share/zoneinfo"))
XKB_RULES = Path(os.environ.get("OPENSHELL_XKB_RULES", "/usr/share/X11/xkb/rules/base.lst"))
MEDIA = Path(os.environ.get("OPENSHELL_MEDIA", "/opt/openshell-media"))


class Supervisor:
    """The session supervisor's system socket: a line each way."""

    def __init__(self, path=None, timeout=10):
        self.path = path or os.environ.get(SYSTEM_SOCKET_ENV, "")
        self.timeout = timeout

    def ask(self, line):
        if "\n" in line or len(line.encode()) > MAX_REQUEST:
            raise ValueError("request too long")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(self.timeout)
            connection.connect(self.path)
            connection.sendall(line.encode() + b"\n")
            answer = b""
            while not answer.endswith(b"\n"):
                chunk = connection.recv(4096)
                if not chunk:
                    break
                answer += chunk
        return answer.decode(errors="replace").rstrip("\n")


def parse_disks(answer):
    """`install-list`'s "disks <device>=<bytes> …": [(device, bytes)]."""
    words = answer.split()
    if not words or words[0] != "disks":
        return []
    disks = []
    for word in words[1:]:
        device, _, size = word.partition("=")
        if device.startswith("/dev/") and size.isdigit():
            disks.append((device, int(size)))
    return disks


def parse_systems(answer):
    """`usb-update-list`'s "systems <device>=<bytes>,<release>,<previous> …"."""
    words = answer.split()
    if not words or words[0] != "systems":
        return []
    systems = []
    for word in words[1:]:
        device, _, rest = word.partition("=")
        fields = rest.split(",")
        if device.startswith("/dev/") and len(fields) == 3 and fields[0].isdigit():
            systems.append({"device": device, "bytes": int(fields[0]), "release": fields[1],
                            "previous": None if fields[2] == "-" else fields[2]})
    return systems


def parse_status(answer):
    """A job's status (install-status, usb-update-status): (state, percent, message)."""
    verb, _, rest = answer.partition(" ")
    if verb == "running":
        percent, _, message = rest.partition(" ")
        return "running", int(percent) if percent.isdigit() else 0, message
    if verb == "done":
        return "done", 100, ""
    if verb == "failed":
        return "failed", 0, rest or "it stopped"
    if verb == "idle":
        return "idle", 0, ""
    return "failed", 0, answer or "no answer"


def size_text(size):
    """Bytes as people read a disk's size: 512 GB, 1.0 TB."""
    for unit, scale in (("TB", 10 ** 12), ("GB", 10 ** 9), ("MB", 10 ** 6)):
        if size >= scale:
            value = size / scale
            return f"{value:.1f} {unit}" if value < 10 else f"{value:.0f} {unit}"
    return f"{size} bytes"


def disk_model(device, sysfs=Path("/sys/block")):
    """What the disk calls itself (its vendor and model), when the kernel says."""
    name = Path(device).name
    words = []
    for field in ("vendor", "model"):
        try:
            text = (sysfs / name / "device" / field).read_text().strip()
        except OSError:
            continue
        if text and text not in words:
            words.append(text)
    return " ".join(words)


def time_zones(zoneinfo=ZONEINFO):
    """The time zones, by region: {"America": ["Sao_Paulo", …], …}, as tzdata's
    zone1970.tab lists them (Settings ▸ Date & time shows the same files)."""
    regions = {}
    try:
        lines = (zoneinfo / "zone1970.tab").read_text().splitlines()
    except OSError:
        lines = []
    for line in lines:
        if line.startswith("#") or not line.strip():
            continue
        fields = line.split("\t")
        if len(fields) < 3 or "/" not in fields[2]:
            continue
        region, _, city = fields[2].partition("/")
        regions.setdefault(region, []).append(city)
    for cities in regions.values():
        cities.sort()
    return dict(sorted(regions.items()))


def current_zone(localtime=Path("/etc/localtime"), zoneinfo=ZONEINFO):
    """The zone /etc/localtime names, if it names one."""
    try:
        target = os.path.realpath(localtime)
    except OSError:
        return None
    root = os.path.realpath(zoneinfo)
    if target.startswith(root + "/"):
        return target[len(root) + 1:]
    return None


def keyboard_layouts(rules=XKB_RULES):
    """xkb's layouts and their variants, from its rules list (base.lst, as
    Settings ▸ Keyboard reads it): ([(layout, description)], {layout: [(variant, description)]})."""
    layouts, variants, section = [], {}, None
    try:
        lines = rules.read_text(errors="replace").splitlines()
    except OSError:
        lines = []
    for line in lines:
        if line.startswith("!"):
            section = line[1:].strip()
            continue
        match = re.match(r"\s*(\S+)\s+(.*)", line)
        if not match:
            continue
        name, description = match.group(1), match.group(2).strip()
        if section == "layout":
            layouts.append((name, description))
        elif section == "variant":
            layout, _, text = description.partition(":")
            variants.setdefault(layout.strip(), []).append((name, text.strip()))
    layouts.sort(key=lambda item: item[1].lower())
    return layouts, variants


def valid_name(name):
    """Whether the account's name can be taken: the supervisor decides
    (settings.rs valid_name: at most 64 bytes, no ":", "," or "="); this
    only spares asking it for what it refuses."""
    return bool(name.strip()) and len(name.encode()) <= 64 and not any(c in name for c in ":,=") and name.isprintable()


def valid_password(password):
    """The same for a password (login.rs valid_password: at most 256 bytes,
    no control characters)."""
    return bool(password) and len(password.encode()) <= 256 and password.isprintable()


def version_key(release):
    """A release name in order: its numbers as numbers (2.10 after 2.9)."""
    return [int(part) if part.isdigit() else part for part in re.split(r"[.\-+_]", release)]


def compare_releases(new, old):
    """-1, 0 or 1: whether `new` is older than, the same as or newer than `old`."""
    a, b = version_key(new), version_key(old)
    try:
        return (a > b) - (a < b)
    except TypeError:
        return (new > old) - (new < old)


def medium_release(media=MEDIA):
    """This medium's release name."""
    try:
        return (media / "release").read_text().strip()
    except OSError:
        return ""


def medium_signed(cmdline=Path("/proc/cmdline")):
    """Whether the medium carries its signed release manifest (what updates need)."""
    try:
        words = cmdline.read_text().split()
    except OSError:
        return False
    return any(word.startswith("openshell.manifest=") for word in words) and \
        any(word.startswith("openshell.manifest_sig=") for word in words)
