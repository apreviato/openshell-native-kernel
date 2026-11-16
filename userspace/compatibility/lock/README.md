# Screen lock

OpenShell Native's screen lock is OpenShell Live's: XSecureLock covers the
screen and grabs the keyboard and mouse, and the OpenShell lock screen is its
authentication module. Only the password check changes: on Live, Debian's PAM
(`authproto_pam`); here, the session supervisor, which keeps the hash of the
password chosen in the installer (`userspace/shell`, `login.rs`). Boot also
opens on this screen: signing in to the installed system uses the same lock
screen.

| File | What it is |
| --- | --- |
| `lock_screen.py`, `lock_view.py`, `lock_auth.py`, `lock_settings.py` | Live's lock screen, unchanged (openshelllive `f27d2db`, `live/config/includes.chroot/usr/local/lib/openshell/`). `initramfs.nix` swaps Debian's paths for Nix's: XSecureLock, `setxkbmap`, the password helper (`openshell-authproto` instead of `authproto_pam`) and the power helper (`openshell-power` instead of `systemctl`), and reads the Settings ▸ Lock screen choices and the account photo from the session's `$HOME`, not from the home folder in `/etc/passwd`. To update, copy Live's files again. |
| `lock.sh` | `openshell-lock`: opens XSecureLock with the lock screen and the same options as Live's `lock.sh`, and stays in the foreground until unlocked. |
| `lock-ready.sh` | XSecureLock runs it once the screen and keyboard are grabbed: it writes the pid to `$XDG_RUNTIME_DIR/openshell-locked`, where the Desktop sees that the screen is locked, and shows the password field. |
| `numlock.py` | Live's Num Lock helper, unchanged: it follows the XKB modifiers (also while XSecureLock holds the keyboard), saves the choice in `$XDG_STATE_HOME/openshell/numlock` and applies it again on the next boot, before the first password. |
| `openshell-wm.sh` | `openshell-wm`, the session's window manager (the supervisor's `wm=`): starts `numlock.py` and then Openbox, in the same process group, before the greeter and the lock screen. |

Locking is done by hostd's lock service (`apps/desktop/hostd/services/lock.ts`):
the Lock button and the idle time chosen in Settings ▸ Lock screen. On Live,
the idle time belongs to `xss-lock`, which needs logind; on Native, the Desktop
reports the idle time to hostd.

Two pieces of the system make this possible on Native:

- `openshell-authproto` (`userspace/shell/src/bin`): speaks XSecureLock's
  authentication protocol and asks the supervisor over the system socket
  (`login <password>`). A wrong password waits 2 s, as at sign-in.
- Kernel patch 0024: the lock screen keeps the password only in locked memory
  (`mlock`), and the kernel now accepts those calls (there is no swap).
