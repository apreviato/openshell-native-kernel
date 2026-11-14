//! The session supervisor of OpenShell Native, started by init as root.
//!
//! It starts the X server, the window manager and the shell launcher as the
//! session user, restarts what fails within a budget, reaps the processes that
//! the session leaves behind and shuts everything down in order. Each boot
//! keeps its logs in its own directory. With a sign-in (login.rs), it runs the
//! greeter before the shell, and checks the lock screen's password. Asked to
//! power off or restart (power.rs), it stops the session and exits with that
//! action's status for init to carry out.

use std::env;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openshell_shell::drives::{self, MountOps, SystemView};
use openshell_shell::firewall::{self, Firewall};
use openshell_shell::install::{Installer, INSTALL_PREFIX, SYSTEM_SOCKET_ENV};
use openshell_shell::login::{parse_request, Phase, Request, GREETER_SOCKET_ENV, MAX_REQUEST};
use openshell_shell::power::PowerAction;
use openshell_shell::settings::{Saved, Setting, SystemFiles};
use openshell_shell::ssh::{self, Ssh};
use openshell_shell::update::{self, Updates};
use openshell_shell::wifi::{self, ControlSockets, Wifi};
use openshell_shell::session::{
    boot_log_dir, CommandLine, HealthWatch, RestartBudget, SessionConfig,
    DESKTOP_HEALTH_ENV, DESKTOP_HEALTH_RESET, DESKTOP_HEALTHY, LOGS_ENV,
};

const USAGE: &str = "Usage: openshell-session <config>";
const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const WNOHANG: i32 = 1;
const PR_SET_CHILD_SUBREAPER: i32 = 36;
const POLL: Duration = Duration::from_millis(100);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const KEPT_BOOTS: usize = 5;
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    fn geteuid() -> u32;
    fn chown(path: *const std::ffi::c_char, uid: u32, gid: u32) -> i32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
    fn capset(header: *mut CapHeader, data: *const CapData) -> i32;
    fn getsockopt(socket: i32, level: i32, name: i32, value: *mut std::ffi::c_void, length: *mut u32) -> i32;
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        fstype: *const std::ffi::c_char,
        flags: u64,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn umount2(target: *const std::ffi::c_char, flags: i32) -> i32;
}

const SOL_SOCKET: i32 = 1;
const SO_PEERCRED: i32 = 17;

#[repr(C)]
#[derive(Default)]
struct PeerCredentials {
    pid: i32,
    uid: u32,
    gid: u32,
}

/// The user of the process at the other end of a Unix socket.
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut credentials = PeerCredentials::default();
    let mut length = std::mem::size_of::<PeerCredentials>() as u32;
    let pointer = (&mut credentials as *mut PeerCredentials).cast();
    if unsafe { getsockopt(stream.as_raw_fd(), SOL_SOCKET, SO_PEERCRED, pointer, &mut length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const CAP_FOWNER: u32 = 3;
const CAP_KILL: u32 = 5;
const CAP_SETGID: u32 = 6;
const CAP_SETUID: u32 = 7;
const CAP_SYS_ADMIN: u32 = 21;
/// What the supervisor keeps once the session's directories exist: starting
/// the session as its user, signalling its processes and removing the stale
/// socket of its display, which belongs to the user.
pub const KEPT_CAPABILITIES: u32 = 1 << CAP_FOWNER | 1 << CAP_KILL | 1 << CAP_SETGID | 1 << CAP_SETUID;
/// Kept but not in effect: mounting the session's drives (drives.rs) raises
/// it for the mount alone.
const HELD_CAPABILITIES: u32 = 1 << CAP_SYS_ADMIN;

fn set_capabilities(effective: u32) -> io::Result<()> {
    let mut header = CapHeader { version: CAPABILITY_VERSION_3, pid: 0 };
    let kept = CapData { effective, permitted: KEPT_CAPABILITIES | HELD_CAPABILITIES, inheritable: 0 };
    let none = CapData { effective: 0, permitted: 0, inheritable: 0 };
    if unsafe { capset(&mut header, [kept, none].as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drop_capabilities() -> io::Result<()> {
    set_capabilities(KEPT_CAPABILITIES)
}

fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// Mounts and unmounts with CAP_SYS_ADMIN in effect for just that call.
struct Mounter;

impl Mounter {
    /// An NTFS volume: ntfs-3g mounts it (FUSE) with CAP_SYS_ADMIN, as
    /// Debian's `mount.ntfs-3g` does as root, and serves it in the
    /// background until it is unmounted (the kernel then ends its
    /// connection and it writes what it kept and leaves).
    fn serve_ntfs(source: &str, target: &Path, options: &str) -> io::Result<()> {
        let mut status = Err(io::Error::from(io::ErrorKind::Other));
        Self::with_admin(|| {
            status = Command::new(drives::NTFS_SERVER)
                .arg(source)
                .arg(target)
                .arg("-o")
                .arg(options)
                .env_clear()
                .env("PATH", "/opt/openshell-compat/bin:/bin:/usr/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            0
        })?;
        match status? {
            exit if exit.success() => Ok(()),
            exit => Err(io::Error::other(format!("ntfs-3g could not mount the volume ({exit})"))),
        }
    }

    fn with_admin(call: impl FnOnce() -> i32) -> io::Result<()> {
        let raised = set_capabilities(KEPT_CAPABILITIES | HELD_CAPABILITIES).is_ok();
        let result = if call() == 0 { Ok(()) } else { Err(io::Error::last_os_error()) };
        if raised {
            let _ = set_capabilities(KEPT_CAPABILITIES);
        }
        result
    }
}

impl MountOps for Mounter {
    fn mount(&mut self, source: &str, target: &Path, fstype: &str, flags: u64, data: &str) -> io::Result<()> {
        if fstype == drives::NTFS_TYPE {
            return Self::serve_ntfs(source, target, data);
        }
        let source = c_path(Path::new(source))?;
        let target = c_path(target)?;
        let fstype = std::ffi::CString::new(fstype).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let data = std::ffi::CString::new(data).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Self::with_admin(|| unsafe { mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), flags, data.as_ptr().cast()) })
    }

    fn unmount(&mut self, target: &Path) -> io::Result<()> {
        let target = c_path(target)?;
        Self::with_admin(|| unsafe { umount2(target.as_ptr(), 0) })
    }
}

extern "C" fn request_stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

/// A line of the supervisor's own log, with the time since the epoch.
struct Journal(File);

impl Journal {
    fn note(&mut self, message: &str) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let _ = writeln!(self.0, "{}.{:03} {message}", now.as_secs(), now.subsec_millis());
        eprintln!("openshell-session: {message}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    XServer,
    WindowManager,
    Greeter,
    Shell,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::XServer => "x",
            Role::WindowManager => "wm",
            Role::Greeter => "greeter",
            Role::Shell => "shell",
        }
    }
}

/// After a wrong password, the greeter waits this long for the answer.
const WRONG_PASSWORD_DELAY: Duration = Duration::from_secs(2);

/// A process of the session, which leads its own process group.
struct Member {
    role: Role,
    pid: i32,
    exited: Option<i32>,
    health: Option<Receiver<Option<Instant>>>,
}

struct Supervisor {
    config: SessionConfig,
    logs: std::path::PathBuf,
    journal: Journal,
    members: Vec<Member>,
    /// Signed in through the greeter (or no sign-in configured).
    signed_in: bool,
    /// The greeter's socket while it runs.
    greeter_socket: Option<std::path::PathBuf>,
    /// The system socket (power, the lock screen's password check and the
    /// live session's installer), with the installer.
    system: Option<(UnixListener, std::path::PathBuf, Option<Installer>)>,
    /// The session asked to power off or restart.
    power: Option<PowerAction>,
    /// The lock screen's password check passed since the last sign-in began.
    unlocked: bool,
}

fn describe(status: i32) -> String {
    if status & 0x7f == 0 {
        format!("status {}", (status >> 8) & 0xff)
    } else {
        format!("signal {}", status & 0x7f)
    }
}

impl Supervisor {
    fn spawn(&mut self, role: Role, command_line: &CommandLine) -> io::Result<()> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o640)
            .open(self.logs.join(format!("{}.log", role.name())))?;
        // The Desktop's Logs show it (the session's group reads this boot's logs).
        std::os::unix::fs::fchown(&log, Some(0), Some(self.config.gid))?;
        let mut command = Command::new(&command_line[0]);
        command
            .args(&command_line[1..])
            .env_clear()
            .env("HOME", &self.config.home)
            .env("DISPLAY", &self.config.display)
            .env("LANG", "C.UTF-8")
            .env(LOGS_ENV, &self.config.log_dir)
            .envs(self.config.env.iter().map(|(name, value)| (name, value)))
            .current_dir(&self.config.home)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?)
            .process_group(0);
        // std drops the supplementary groups when it changes the user as root.
        command.uid(self.config.uid).gid(self.config.gid);
        if let (Role::Greeter, Some(socket)) = (role, &self.greeter_socket) {
            command.env(GREETER_SOCKET_ENV, socket);
        }
        // The shell asks it for power, settings and updates; the boot lock
        // (a greeter) checks the password on it.
        if let (Role::Shell | Role::Greeter, Some((_, socket, _))) = (role, &self.system) {
            command.env(SYSTEM_SOCKET_ENV, socket);
        }
        if role == Role::Shell && self.config.confirm.is_some() {
            command.env(DESKTOP_HEALTH_ENV, "1").stdout(Stdio::piped());
        } else {
            command.env_remove(DESKTOP_HEALTH_ENV);
        }
        let mut child = command.spawn()?;
        let health = child.stdout.take().map(|stdout| {
            let (send, receive) = mpsc::channel();
            thread::spawn(move || {
                let mut log = log;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let event = match line.as_str() {
                        DESKTOP_HEALTHY => Some(Instant::now()),
                        DESKTOP_HEALTH_RESET => None,
                        _ => {
                            let _ = writeln!(log, "{line}");
                            continue;
                        }
                    };
                    if send.send(event).is_err() {
                        return;
                    }
                }
                let _ = send.send(None);
            });
            receive
        });
        let pid = child.id() as i32;
        // The supervisor reaps every process with waitpid, the session's
        // orphans included.
        drop(child);
        self.journal.note(&format!("started {} (pid {pid})", role.name()));
        self.members.retain(|member| member.role != role);
        self.members.push(Member { role, pid, exited: None, health });
        Ok(())
    }

    /// Reaps every exited child. The session's members get their status;
    /// orphans reparented to the supervisor are only noted.
    fn reap(&mut self) {
        loop {
            let mut status = 0;
            let pid = unsafe { waitpid(-1, &mut status, WNOHANG) };
            if pid <= 0 {
                return;
            }
            if let Some(member) = self.members.iter_mut().find(|member| member.pid == pid) {
                member.exited = Some(status);
                let role = member.role.name();
                self.journal.note(&format!("{role} (pid {pid}) exited with {}", describe(status)));
            } else {
                self.journal.note(&format!("reaped orphan pid {pid} ({})", describe(status)));
            }
        }
    }

    fn exited(&self, role: Role) -> Option<i32> {
        self.members.iter().find(|member| member.role == role).and_then(|member| member.exited)
    }

    /// Stops a member and the rest of its process group, then reaps it.
    fn stop(&mut self, role: Role) {
        let Some(pid) = self.members.iter().find(|member| member.role == role).map(|member| member.pid) else {
            return;
        };
        unsafe { kill(-pid, SIGTERM) };
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline && unsafe { kill(-pid, 0) } == 0 {
            self.reap();
            thread::sleep(POLL);
        }
        if unsafe { kill(-pid, 0) } == 0 {
            self.journal.note(&format!("{} did not stop; killing its group", role.name()));
            unsafe { kill(-pid, SIGKILL) };
            thread::sleep(POLL);
        }
        self.reap();
        self.members.retain(|member| member.role != role);
    }

    fn stop_all(&mut self) {
        for role in [Role::Shell, Role::Greeter, Role::WindowManager, Role::XServer] {
            self.stop(role);
        }
    }

    fn wait_for_x(&mut self) -> Result<(), Box<dyn Error>> {
        let socket = self.config.x_socket();
        let deadline = Instant::now() + READY_TIMEOUT;
        while !socket.exists() {
            self.reap();
            if STOP.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            if let Some(status) = self.exited(Role::XServer) {
                return Err(format!("the X server exited with {}", describe(status)).into());
            }
            if Instant::now() > deadline {
                return Err("the X server did not create its socket".into());
            }
            thread::sleep(POLL);
        }
        self.journal.note("x is ready");
        Ok(())
    }

    /// Starts X and the window manager, then the shell.
    fn start(&mut self) -> Result<(), Box<dyn Error>> {
        release_display(&self.config)?;
        let x_server = self.config.x_server.clone();
        self.spawn(Role::XServer, &x_server)?;
        self.wait_for_x()?;
        if let Some(window_manager) = self.config.window_manager.clone() {
            self.spawn(Role::WindowManager, &window_manager)?;
        }
        if !self.signed_in {
            self.sign_in()?;
        }
        // Powered off or restarted from the lock screen: no shell (run ends).
        if self.power.is_some() {
            return Ok(());
        }
        let shell = self.config.shell.clone();
        self.spawn(Role::Shell, &shell)?;
        Ok(())
    }

    /// Opens the system socket (install.rs): power, the lock screen's
    /// password check and the live session's installer.
    fn open_system_socket(&mut self) -> io::Result<()> {
        let folder = std::path::PathBuf::from(format!("/tmp/openshell-system-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::DirBuilder::new().mode(0o711).create(&folder)?;
        let socket = folder.join("system.sock");
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        // The installer hashes the new password with the sign-in's tool.
        let password_tool = self.config.login.as_ref().map_or_else(
            || vec![openshell_shell::login::DEFAULT_PASSWORD_TOOL.to_owned()],
            |login| login.password_tool.clone(),
        );
        self.system = Some((listener, socket, self.config.installer.clone().map(|command| Installer::new(command, password_tool))));
        Ok(())
    }

    /// Answers what the session asked on the system socket, from its user only.
    fn serve_system(&mut self) {
        let Some((listener, _, installer)) = &self.system else { return };
        while let Ok((stream, _)) = listener.accept() {
            if peer_uid(&stream).ok() != Some(self.config.uid) {
                continue;
            }
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let _ = BufReader::new(&stream).take(MAX_REQUEST as u64 + 1).read_until(b'\n', &mut request);
            let line = String::from_utf8_lossy(&request);
            let line = line.strip_suffix('\n').unwrap_or(&line);
            let unlock = matches!(parse_request(line), Some(Request::State | Request::Login(_)));
            let answer = if unlock && request.len() <= MAX_REQUEST {
                // The lock screen's password check (openshell-authproto).
                match &self.config.login {
                    Some(login) => {
                        let answer = login.answer(line, Phase::Unlock).unwrap_or("invalid");
                        match answer {
                            "ok" => {
                                self.journal.note("unlocked");
                                self.unlocked = true;
                            }
                            "wrong" => {
                                self.journal.note("a wrong password at the lock screen");
                                thread::sleep(WRONG_PASSWORD_DELAY);
                            }
                            _ => {}
                        }
                        answer.to_owned()
                    }
                    None => "unavailable".to_owned(),
                }
            } else if Setting::is_request(line) {
                match Setting::from_request(line) {
                    Some(setting) => {
                        let applied = SystemFiles::default().apply(&setting, self.config.uid).and_then(|()| {
                            self.config.settings_dir.as_ref().map_or(Ok(()), |folder| Saved(folder.clone()).save(&setting))
                        });
                        match applied {
                            Ok(()) => {
                                // A held brightness key would fill the journal.
                                if !matches!(setting, Setting::Brightness(_)) {
                                    self.journal.note(&format!("set {setting:?}"));
                                }
                                "ok".to_owned()
                            }
                            Err(error) => {
                                self.journal.note(&format!("could not set {setting:?}: {error}"));
                                "failed".to_owned()
                            }
                        }
                    }
                    None => "invalid".to_owned(),
                }
            } else if update::is_request(line) {
                match &self.config.updates_dir {
                    Some(folder) => {
                        let answer = Updates(folder.clone()).answer(line).unwrap_or_else(|error| format!("failed {error}"));
                        if answer == "ok" && line != update::UPDATE_STATUS {
                            self.journal.note(&format!("updates: {line}"));
                        }
                        answer
                    }
                    None => "unavailable".to_owned(),
                }
            } else if firewall::is_request(line) {
                let answer = Firewall::new(self.config.settings_dir.clone()).answer(line);
                if line != "firewall status" {
                    self.journal.note(&format!("{line}: {answer}"));
                }
                answer
            } else if ssh::is_request(line) {
                let answer = Ssh::new(self.config.settings_dir.clone()).answer(line);
                if line != "ssh status" {
                    self.journal.note(&format!("{line}: {answer}"));
                }
                answer
            } else if wifi::is_request(line) {
                // Only a whole line: one cut at the limit could still read
                // as a request, with another password.
                if request.last() != Some(&b'\n') {
                    "invalid".to_owned()
                } else {
                    let answer = Wifi::new(self.config.settings_dir.clone()).answer(line, &mut ControlSockets);
                    // What changes something, by its verb alone: the line
                    // may hold a password.
                    let verb = line.split(' ').nth(1).unwrap_or("");
                    if !matches!(verb, "status" | "networks" | "scan") {
                        self.journal.note(&format!("wifi {verb}: {}", answer.split(' ').next().unwrap_or("")));
                    }
                    answer
                }
            } else if drives::is_request(line) {
                // /media/<user>, as udisks2 mounts the session's drives.
                let user = self.config.home.file_name().and_then(|name| name.to_str()).unwrap_or("user").to_owned();
                let answer = SystemView::default().answer(line, &user, self.config.uid, self.config.gid, &mut Mounter);
                if line != drives::DRIVES_REQUEST {
                    self.journal.note(&format!("{line}: {answer}"));
                }
                answer
            } else if PowerAction::is_request(line) {
                match PowerAction::from_request(line) {
                    Some(action) => {
                        self.journal.note(&format!("the session asked to {}", action.verb()));
                        self.power = Some(action);
                        "ok".to_owned()
                    }
                    None => "invalid".to_owned(),
                }
            } else if let Some(installer) = installer {
                installer.answer(line)
            } else if line.starts_with(INSTALL_PREFIX) {
                // Only the live medium installs (installed-session.sh).
                "unavailable".to_owned()
            } else {
                "invalid".to_owned()
            };
            if answer == "started" {
                self.journal.note(&format!("the installer started ({line})"));
            }
            let _ = writeln!(&stream, "{answer}");
        }
    }

    /// Runs the greeter until the person signs in (login.rs). It answers the
    /// greeter's requests on a socket, from the session user's processes only
    /// (the supervisor no longer has CAP_CHOWN to give the socket away, so it
    /// checks each peer); a greeter that fails starts again, within a budget.
    fn sign_in(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(login) = self.config.login.clone() else {
            self.signed_in = true;
            return Ok(());
        };
        if let Some(lock) = login.lock.clone() {
            if login.stored()?.is_some() {
                return self.unlock_to_sign_in(&lock);
            }
        }
        let folder = std::path::PathBuf::from(format!("/tmp/openshell-greeter-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::DirBuilder::new().mode(0o711).create(&folder)?;
        let socket = folder.join("greeter.sock");
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        self.greeter_socket = Some(socket);
        let mut budget = RestartBudget::new(5, Duration::from_secs(60));
        self.spawn(Role::Greeter, &login.greeter)?;
        let result: Result<(), Box<dyn Error>> = loop {
            self.reap();
            if STOP.load(Ordering::Relaxed) {
                break Err("stopped".into());
            }
            if let Some(status) = self.exited(Role::XServer) {
                break Err(format!("the X server exited with {} before signing in", describe(status)).into());
            }
            if self.exited(Role::Greeter).is_some() {
                let Some(delay) = budget.next_delay(Instant::now()) else { break Err("the greeter keeps failing".into()) };
                self.stop(Role::Greeter);
                thread::sleep(delay);
                self.spawn(Role::Greeter, &login.greeter)?;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if peer_uid(&stream).ok() != Some(self.config.uid) {
                        self.journal.note("refused a sign-in request from another user");
                        continue;
                    }
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    // One line: its newline ends the request.
                    let mut request = Vec::new();
                    let _ = BufReader::new(&stream).take(MAX_REQUEST as u64 + 1).read_until(b'\n', &mut request);
                    let line = String::from_utf8_lossy(&request);
                    let line = line.strip_suffix('\n').unwrap_or(&line);
                    let answer = if request.len() > MAX_REQUEST { "invalid" } else { login.answer(line, Phase::SignIn)? };
                    match answer {
                        "ok" => self.journal.note("signed in"),
                        "wrong" => {
                            self.journal.note("a wrong password at the greeter");
                            thread::sleep(WRONG_PASSWORD_DELAY);
                        }
                        _ => {}
                    }
                    let _ = writeln!(stream, "{answer}");
                    if answer == "ok" {
                        break Ok(());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(error) => break Err(error.into()),
            }
        };
        self.greeter_socket = None;
        let _ = fs::remove_dir_all(&folder);
        if result.is_ok() {
            // The greeter has its answer; the Desktop takes over the screen.
            self.stop(Role::Greeter);
            self.signed_in = true;
        }
        result
    }

    /// Signs in through the screen lock, as OpenShell Live's session does
    /// (xsession.sh, openshell_wait_for_unlock): the lock runs as a greeter
    /// until it ends with the password checked here (`unlocked`, from its
    /// password check on the system socket) and its own success. A lock that
    /// fails or is killed starts again: the Desktop never opens without it.
    fn unlock_to_sign_in(&mut self, lock: &CommandLine) -> Result<(), Box<dyn Error>> {
        let mut budget = RestartBudget::new(5, Duration::from_secs(60));
        self.unlocked = false;
        self.spawn(Role::Greeter, lock)?;
        loop {
            self.reap();
            self.serve_system();
            if STOP.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            if let Some(status) = self.exited(Role::XServer) {
                return Err(format!("the X server exited with {} before signing in", describe(status)).into());
            }
            if self.power.is_some() {
                self.stop(Role::Greeter);
                return Ok(());
            }
            if let Some(status) = self.exited(Role::Greeter) {
                self.members.retain(|member| member.role != Role::Greeter);
                if status == 0 && self.unlocked {
                    self.journal.note("signed in");
                    self.signed_in = true;
                    return Ok(());
                }
                self.journal.note(&format!("the lock ended with {} before the password; locking again", describe(status)));
                let delay = budget.next_delay(Instant::now()).ok_or("the lock keeps failing")?;
                thread::sleep(delay);
                self.unlocked = false;
                self.spawn(Role::Greeter, lock)?;
            }
            thread::sleep(POLL);
        }
    }

    /// The session is healthy: the release the computer booted is confirmed
    /// (the A/B transaction, distro/ab), once per boot. A failure is recorded;
    /// the release then stays on trial and the next boots fall back.
    fn confirm_boot(&mut self) {
        let Some(confirm) = self.config.confirm.clone() else { return };
        match Command::new(&confirm[0]).args(&confirm[1..]).env_clear().stdin(Stdio::null()).status() {
            Ok(status) if status.success() => self.journal.note("the session is healthy; confirmed the boot"),
            Ok(status) => self.journal.note(&format!("confirming the boot failed ({status})")),
            Err(error) => self.journal.note(&format!("confirming the boot failed ({error})")),
        }
    }

    /// Runs the session until the shell ends it, it asks to power off or
    /// restart, a stop is requested or failures exceed the budget.
    fn run(&mut self) -> Result<Option<PowerAction>, Box<dyn Error>> {
        let mut session_budget = RestartBudget::new(3, Duration::from_secs(300));
        let mut shell_budget = RestartBudget::new(5, Duration::from_secs(60));
        let mut wm_budget = RestartBudget::new(5, Duration::from_secs(60));
        let mut health = HealthWatch::new(self.config.confirm_after);
        let mut confirmation_attempted = false;
        let mut health_seen = false;
        self.start()?;
        while !STOP.load(Ordering::Relaxed) {
            self.reap();
            self.serve_system();
            if let Some(action) = self.power {
                return Ok(Some(action));
            }
            if let Some(status) = self.exited(Role::XServer) {
                // Without X, the whole session starts over.
                self.journal.note(&format!("x failed ({}); restarting the session", describe(status)));
                self.stop_all();
                let delay = session_budget.next_delay(Instant::now()).ok_or("the X server keeps failing")?;
                thread::sleep(delay);
                self.start()?;
                health.reset();
                continue;
            }
            if let Some(status) = self.exited(Role::Shell) {
                if status == 0 {
                    self.journal.note("the shell ended the session");
                    return Ok(None);
                }
                let delay = shell_budget.next_delay(Instant::now()).ok_or("the shell keeps failing")?;
                self.journal.note(&format!("restarting the shell in {} ms", delay.as_millis()));
                self.stop(Role::Shell);
                thread::sleep(delay);
                // As on Live: a Desktop that restarts opens behind the lock again.
                if let Some(lock) = self.config.login.as_ref().and_then(|login| login.lock.clone()) {
                    self.unlock_to_sign_in(&lock)?;
                    if self.power.is_some() {
                        continue;
                    }
                }
                let shell = self.config.shell.clone();
                self.spawn(Role::Shell, &shell)?;
                health.reset();
            }
            if self.exited(Role::WindowManager).is_some() {
                if let Some(delay) = wm_budget.next_delay(Instant::now()) {
                    self.stop(Role::WindowManager);
                    thread::sleep(delay);
                    if let Some(window_manager) = self.config.window_manager.clone() {
                        self.spawn(Role::WindowManager, &window_manager)?;
                        health.reset();
                        if let Some(events) = self.members.iter().find_map(|member| member.health.as_ref()) {
                            for _ in events.try_iter() {}
                        }
                    }
                }
            } else if !confirmation_attempted {
                if let Some(events) = self.members.iter().find_map(|member| member.health.as_ref()) {
                    for event in events.try_iter() {
                        match event {
                            Some(at) => {
                                if !health_seen {
                                    self.journal.note("the Desktop reported its first successful health check");
                                    health_seen = true;
                                }
                                health.report(at);
                            }
                            None => health.reset(),
                        }
                    }
                }
                if health.healthy(Instant::now()) {
                    confirmation_attempted = true;
                    self.confirm_boot();
                }
            }
            thread::sleep(POLL);
        }
        self.journal.note("stop requested");
        Ok(None)
    }
}

/// Removes the socket and lock of a display whose X server is gone. A display
/// in use by a running server is an error.
fn release_display(config: &SessionConfig) -> Result<(), Box<dyn Error>> {
    let number = &config.display[1..];
    let lock = format!("/tmp/.X{number}-lock");
    if let Ok(text) = fs::read_to_string(&lock) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            if pid > 0 && unsafe { kill(pid, 0) } == 0 {
                return Err(format!("display {} is in use by pid {pid}", config.display).into());
            }
        }
        fs::remove_file(&lock)?;
    }
    match fs::remove_file(config.x_socket()) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn prepare_home(config: &SessionConfig) -> Result<(), Box<dyn Error>> {
    // The X servers of every user put their sockets here.
    fs::DirBuilder::new().recursive(true).create("/tmp/.X11-unix")?;
    fs::set_permissions("/tmp/.X11-unix", std::os::unix::fs::PermissionsExt::from_mode(0o1777))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&config.home)?;
    let path = std::ffi::CString::new(config.home.as_os_str().as_encoded_bytes())?;
    if unsafe { chown(path.as_ptr(), config.uid, config.gid) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 1 || args[0] == "--help" {
        eprintln!("{USAGE}");
        return if args.len() == 1 { ExitCode::SUCCESS } else { ExitCode::from(2) };
    }
    let result = (|| -> Result<Option<PowerAction>, Box<dyn Error>> {
        if unsafe { geteuid() } != 0 {
            return Err("The supervisor runs as root and starts the session as its user".into());
        }
        let config = SessionConfig::parse(&fs::read_to_string(&args[0])?)?;
        for number in [SIGINT, SIGTERM] {
            if unsafe { signal(number, request_stop) } == usize::MAX {
                return Err(io::Error::last_os_error().into());
            }
        }
        // Processes that the session leaves behind are reparented here and reaped.
        if unsafe { prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        prepare_home(&config)?;
        let logs = boot_log_dir(Path::new(&config.log_dir), KEPT_BOOTS)?;
        // This boot's logs, for the Desktop's Logs: root writes them, the
        // session's group reads them (OpenShell Live's journal, which its
        // desktop user reads).
        std::os::unix::fs::chown(&logs, Some(0), Some(config.gid))?;
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o750))?;
        let journal_file = OpenOptions::new().create(true).append(true).mode(0o640).open(logs.join("session.log"))?;
        std::os::unix::fs::fchown(&journal_file, Some(0), Some(config.gid))?;
        let journal = Journal(journal_file);
        let mut supervisor = Supervisor { config, logs, journal, members: Vec::new(), signed_in: false, greeter_socket: None, system: None, power: None, unlocked: false };
        supervisor.open_system_socket()?;
        // The time zone and the account's name chosen in Settings, before the session.
        if let Some(folder) = supervisor.config.settings_dir.clone() {
            for setting in Saved(folder).load() {
                if let Err(error) = SystemFiles::default().apply(&setting, supervisor.config.uid) {
                    supervisor.journal.note(&format!("could not set {setting:?}: {error}"));
                }
            }
        }
        supervisor.journal.note(&format!("session logs in {}", supervisor.logs.display()));
        // Root keeps only what the supervision needs.
        drop_capabilities()?;
        supervisor.journal.note(&format!("kept capabilities {KEPT_CAPABILITIES:#x}"));
        let outcome = supervisor.run();
        if let Err(error) = &outcome {
            supervisor.journal.note(&format!("session failed: {error}"));
        }
        supervisor.stop_all();
        supervisor.journal.note("session stopped");
        outcome
    })();
    match result {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(action)) => ExitCode::from(action.exit_status()),
        Err(error) => {
            eprintln!("openshell-session: {error}");
            ExitCode::FAILURE
        }
    }
}
