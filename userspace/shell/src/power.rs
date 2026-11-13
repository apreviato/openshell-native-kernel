//! Powering off and restarting the computer from the session.
//!
//! The session asks the supervisor on its system socket (install.rs):
//!
//! ```text
//! power poweroff     → "ok", then the session stops
//! power reboot       → "ok", then the session stops
//! ```
//!
//! Anything else after `power` is "invalid" (there is no suspend). The
//! supervisor keeps no CAP_SYS_BOOT: it stops the session and exits with
//! POWEROFF_STATUS or REBOOT_STATUS, and init (installed-session.sh) writes
//! the disks out and does it.

/// The request's verb on the system socket.
pub const POWER_REQUEST: &str = "power";
/// The supervisor's exit status for each action (installed-session.sh).
pub const POWEROFF_STATUS: u8 = 64;
pub const REBOOT_STATUS: u8 = 65;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    PowerOff,
    Reboot,
}

impl PowerAction {
    /// The action of `power <verb>`, None for any other line.
    pub fn from_request(line: &str) -> Option<Self> {
        match line.strip_prefix(POWER_REQUEST)?.strip_prefix(' ')? {
            "poweroff" => Some(PowerAction::PowerOff),
            "reboot" => Some(PowerAction::Reboot),
            _ => None,
        }
    }

    /// Whether the line is a `power` request at all, valid or not.
    pub fn is_request(line: &str) -> bool {
        line == POWER_REQUEST || line.starts_with("power ")
    }

    pub fn verb(self) -> &'static str {
        match self {
            PowerAction::PowerOff => "poweroff",
            PowerAction::Reboot => "reboot",
        }
    }

    pub fn exit_status(self) -> u8 {
        match self {
            PowerAction::PowerOff => POWEROFF_STATUS,
            PowerAction::Reboot => REBOOT_STATUS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_power_requests() {
        assert_eq!(PowerAction::from_request("power poweroff"), Some(PowerAction::PowerOff));
        assert_eq!(PowerAction::from_request("power reboot"), Some(PowerAction::Reboot));
        for other in ["power suspend", "power", "power  reboot", "powerreboot", "reboot"] {
            assert_eq!(PowerAction::from_request(other), None, "{other}");
        }
        assert!(PowerAction::is_request("power suspend") && PowerAction::is_request("power"));
        assert!(!PowerAction::is_request("powers"));
        for action in [PowerAction::PowerOff, PowerAction::Reboot] {
            assert_eq!(PowerAction::from_request(&format!("{POWER_REQUEST} {}", action.verb())), Some(action));
        }
    }
}
