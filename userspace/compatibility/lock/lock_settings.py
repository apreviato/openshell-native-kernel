"""Lock screen preferences from Settings ▸ Lock screen.

OpenShell's hostd writes ~/.config/openshell/lock.json, and the picture
chosen for the background next to it; the lock screen only reads them.
Missing, unreadable or out-of-range values fall back to defaults, so a
hand-edited file can change what is shown but never how unlocking works.
"""
import json
from pathlib import Path
import re

DEFAULTS = {
    'screenOffWhenLocked': 60,
    'clock24h': True,
    'showName': True,
    'showBattery': True,
    'powerButtons': True,
    # The look (Settings ▸ Lock screen ▸ Look); these draw the usual screen.
    'background': 'default',
    'backgroundColor': '#16202e',
    'backgroundColor2': '#3b2a55',
    'pictureDim': 35,
    'textColor': '#e6e8eb',
    'accentColor': '#e6e8eb',
    'clockWeight': 'light',
    'showDate': True,
}
CHOICES = {
    'background': ('default', 'color', 'gradient', 'picture'),
    'clockWeight': ('light', 'regular', 'bold'),
}
COLORS = ('backgroundColor', 'backgroundColor2', 'textColor', 'accentColor')
COLOR = re.compile(r'#[0-9a-fA-F]{6}')
MAX_SECONDS = 4 * 3600
# Largest value of the whole numbers that are not seconds.
LIMITS = {'pictureDim': 80}
MAX_PICTURE = 10 * 1024 * 1024


def path(home):
    return Path(home) / '.config/openshell/lock.json'


def picture_path(home):
    return Path(home) / '.config/openshell/lock-background'


def load(home):
    values = dict(DEFAULTS)
    try:
        with path(home).open('rb') as handle:
            raw = json.loads(handle.read(65536))
    except (OSError, ValueError):
        return values
    if not isinstance(raw, dict):
        return values
    for key, default in DEFAULTS.items():
        value = raw.get(key)
        if key in CHOICES:
            if value in CHOICES[key]:
                values[key] = value
        elif key in COLORS:
            if isinstance(value, str) and COLOR.fullmatch(value):
                values[key] = value.lower()
        elif isinstance(default, bool):
            if isinstance(value, bool):
                values[key] = value
        elif isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= LIMITS.get(key, MAX_SECONDS):
            values[key] = value
    return values


def battery(root='/'):
    """(percent, charging) of the computer's own batteries, or None."""
    supplies = Path(root) / 'sys/class/power_supply'
    found = []
    try:
        entries = sorted(supplies.iterdir())
    except OSError:
        return None
    for supply in entries:
        def read(name):
            try:
                return (supply / name).read_text().strip()
            except OSError:
                return ''
        # Peripheral batteries (a wireless mouse) report scope=Device.
        if read('type') != 'Battery' or read('scope') == 'Device' or read('present') == '0':
            continue
        try:
            found.append((int(read('capacity')), read('status').lower()))
        except ValueError:
            continue
    if not found:
        return None
    percent = round(sum(item[0] for item in found) / len(found))
    return max(0, min(100, percent)), any(item[1] in ('charging', 'full') for item in found)
