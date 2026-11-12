"""Runs a command as the unprivileged user 65534 without supplementary groups.

Usage: python -I unprivileged.py <program> [arguments...]
"""

import os
import sys

UNPRIVILEGED = 65534

if len(sys.argv) < 2:
    sys.exit(__doc__.strip())
os.setgroups([])
os.setresgid(UNPRIVILEGED, UNPRIVILEGED, UNPRIVILEGED)
os.setresuid(UNPRIVILEGED, UNPRIVILEGED, UNPRIVILEGED)
os.execv(sys.argv[1], sys.argv[1:])
