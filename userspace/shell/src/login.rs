//! Signing in before the session (ARCHITECTURE-CEF.md, next step 5).
//!
//! The supervisor, as root, keeps the password's hash in a file only it can
//! read, and starts the greeter on the session's X display as the session
//! user. The greeter (the Desktop's sign-in page) asks the supervisor over a
//! Unix socket, one request per connection, each line ended by its newline:
//!
//! ```text
//! state             → "create" (no password yet), "login" or "unlock"
//! login <password>  → "ok" or "wrong"
//! create <password> → "ok", "exists" or "invalid"
//! ```
//!
//! The shell starts only after an "ok". The lock screen (XSecureLock with
//! OpenShell's lock screen, userspace/lock) checks the same password through
//! openshell-authproto, with "state" and "login" on the supervisor's system
//! socket (install.rs): "state" answers "unlock" there, and no password is
//! created. Hashes are SHA-512 crypt (`$6$`),
//! made and checked with BusyBox's `cryptpw`, which reads the password from a
//! file descriptor, never from its arguments.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::session::CommandLine;

/// The environment variable that names the supervisor's socket to the greeter.
pub const GREETER_SOCKET_ENV: &str = "OPENSHELL_GREETER_SOCKET";
/// The longest password the greeter may send, and the longest request line.
pub const MAX_PASSWORD: usize = 256;
/// Room for the longest verb before a password ("install-password ").
pub const MAX_REQUEST: usize = MAX_PASSWORD + 32;
/// BusyBox's cryptpw where the system image installs BusyBox.
pub const DEFAULT_PASSWORD_TOOL: &str = "/bin/cryptpw";
/// What the password is for: signing in before the shell (the greeter), or
/// unlocking it (the lock screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    SignIn,
    Unlock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginConfig {
    pub greeter: CommandLine,
    pub password_file: PathBuf,
    pub password_tool: CommandLine,
    /// The screen lock (openshell-lock: XSecureLock with Live's lock screen).
    /// With a password, signing in is unlocking it, as on OpenShell Live; the
    /// greeter remains for creating the first password.
    pub lock: Option<CommandLine>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    State,
    Login(String),
    Create(String),
}

/// A password the supervisor takes: not empty, not too long, no control
/// characters. It may hold spaces.
pub fn valid_password(password: &str) -> bool {
    !password.is_empty() && password.len() <= MAX_PASSWORD && !password.chars().any(char::is_control)
}

/// One request line, without its newline.
pub fn parse_request(line: &str) -> Option<Request> {
    let (verb, password) = line.split_once(' ').unwrap_or((line, ""));
    let valid = valid_password;
    match verb {
        "state" if password.is_empty() => Some(Request::State),
        "login" if valid(password) => Some(Request::Login(password.to_owned())),
        "create" if valid(password) => Some(Request::Create(password.to_owned())),
        _ => None,
    }
}

/// `$6$<salt>$<hash>`: the salt, or None for any other form (rounds included).
pub fn hash_salt(stored: &str) -> Option<&str> {
    let mut parts = stored.split('$');
    match (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(""), Some("6"), Some(salt), Some(hash), None)
            if !salt.is_empty() && !salt.contains('=') && !hash.is_empty() =>
        {
            Some(salt)
        }
        _ => None,
    }
}

/// Runs the password tool on `password`, with `salt` to check a stored hash.
fn crypt(tool: &CommandLine, password: &str, salt: Option<&str>) -> io::Result<String> {
    let mut command = Command::new(&tool[0]);
    command.args(&tool[1..]).args(["-m", "sha512", "-P", "0"]);
    if let Some(salt) = salt {
        command.args(["-S", salt]);
    }
    let mut child = command.env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    child.stdin.take().expect("piped stdin").write_all(password.as_bytes())?;
    let output = child.wait_with_output()?;
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || hash_salt(&hash).is_none() {
        return Err(io::Error::other("the password tool gave no SHA-512 crypt hash"));
    }
    Ok(hash)
}

/// A new password's SHA-512 crypt hash, as the password file keeps it (the installer's).
pub fn hash_password(tool: &CommandLine, password: &str) -> io::Result<String> {
    crypt(tool, password, None)
}

/// Equal lengths and bytes, in time that does not depend on where they differ.
fn same(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

impl LoginConfig {
    /// The stored hash, or None before a password was created.
    pub fn stored(&self) -> io::Result<Option<String>> {
        match fs::read_to_string(&self.password_file) {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn check(&self, password: &str) -> io::Result<bool> {
        let Some(stored) = self.stored()? else { return Ok(false) };
        let Some(salt) = hash_salt(&stored) else { return Ok(false) };
        Ok(same(crypt(&self.password_tool, password, Some(salt))?.as_bytes(), stored.as_bytes()))
    }

    /// Records the first password; never replaces one.
    pub fn create(&self, password: &str) -> io::Result<bool> {
        if self.stored()?.is_some() {
            return Ok(false);
        }
        let hash = crypt(&self.password_tool, password, None)?;
        let folder = self.password_file.parent().unwrap_or(Path::new("/"));
        fs::DirBuilder::new().recursive(true).mode(0o700).create(folder)?;
        let partial = self.password_file.with_extension("partial");
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&partial)?;
        file.write_all(format!("{hash}\n").as_bytes())?;
        file.sync_all()?;
        fs::rename(&partial, &self.password_file)?;
        fs::File::open(folder)?.sync_all()?;
        Ok(true)
    }

    /// The reply to one request line.
    pub fn answer(&self, line: &str, phase: Phase) -> io::Result<&'static str> {
        Ok(match parse_request(line) {
            Some(Request::State) if phase == Phase::Unlock => "unlock",
            Some(Request::Create(_)) if phase == Phase::Unlock => "invalid",
            Some(Request::State) => {
                if self.stored()?.is_some() {
                    "login"
                } else {
                    "create"
                }
            }
            Some(Request::Login(password)) => {
                if self.check(&password)? {
                    "ok"
                } else {
                    "wrong"
                }
            }
            Some(Request::Create(password)) => {
                if self.create(&password)? {
                    "ok"
                } else {
                    "exists"
                }
            }
            None => "invalid",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_requests() {
        assert_eq!(parse_request("state"), Some(Request::State));
        assert_eq!(parse_request("login two words"), Some(Request::Login("two words".into())));
        assert_eq!(parse_request("create x"), Some(Request::Create("x".into())));
        for bad in ["state x", "login", "login ", "create a\u{7}b", "delete x", ""] {
            assert_eq!(parse_request(bad), None, "{bad:?}");
        }
        assert_eq!(parse_request(&format!("login {}", "a".repeat(MAX_PASSWORD + 1))), None);
    }

    #[test]
    fn finds_the_salt_of_sha512_crypt_only() {
        assert_eq!(hash_salt("$6$abcdefgh$xyz"), Some("abcdefgh"));
        for bad in ["$5$salt$hash", "$6$rounds=5000$salt$hash", "$6$$hash", "$6$salt$", "plain", "$6$a$b$c"] {
            assert_eq!(hash_salt(bad), None, "{bad}");
        }
    }

    /// A stand-in for cryptpw: `$6$<salt>$<password in hex>`, salt `fixed` by default.
    fn fake_tool(folder: &Path) -> CommandLine {
        let script = folder.join("cryptpw");
        fs::write(
            &script,
            "#!/bin/sh\nsalt=fixed\nwhile [ $# -gt 0 ]; do [ \"$1\" = -S ] && salt=$2; shift; done\n\
             read -r password\nprintf '$6$%s$%s\\n' \"$salt\" \"$(printf %s \"$password\" | od -An -tx1 | tr -d ' \\n')\"\n",
        )
        .unwrap();
        vec!["/bin/sh".into(), script.to_string_lossy().into_owned()]
    }

    impl LoginConfig {
        fn answer_sign_in(&self, line: &str) -> io::Result<&'static str> {
            self.answer(line, Phase::SignIn)
        }
    }

    #[test]
    fn creates_once_and_checks_passwords() {
        let folder = std::env::temp_dir().join(format!("openshell-login-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let config = LoginConfig {
            greeter: vec!["/bin/true".into()],
            password_file: folder.join("account").join("password"),
            password_tool: fake_tool(&folder),
            lock: None,
        };
        assert_eq!(config.answer_sign_in("state").unwrap(), "create");
        assert_eq!(config.answer_sign_in("login secret").unwrap(), "wrong");
        // `read` without a newline still reads the password (cryptpw -P 0 reads to EOF).
        assert_eq!(config.answer_sign_in("create secret").unwrap(), "ok");
        assert_eq!(config.answer_sign_in("state").unwrap(), "login");
        assert_eq!(config.answer_sign_in("create other").unwrap(), "exists");
        assert_eq!(config.answer_sign_in("login secret").unwrap(), "ok");
        assert_eq!(config.answer_sign_in("login secreT").unwrap(), "wrong");
        assert_eq!(config.answer_sign_in("nonsense").unwrap(), "invalid");
        let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&config.password_file).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        // Locked: the same password unlocks, and none is created.
        assert_eq!(config.answer("state", Phase::Unlock).unwrap(), "unlock");
        assert_eq!(config.answer("create other", Phase::Unlock).unwrap(), "invalid");
        assert_eq!(config.answer("login secreT", Phase::Unlock).unwrap(), "wrong");
        assert_eq!(config.answer("login secret", Phase::Unlock).unwrap(), "ok");
        fs::remove_dir_all(&folder).unwrap();
    }
}
