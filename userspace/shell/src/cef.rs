use std::ffi::{c_char, CString};
use std::io::{self, BufRead, Read};
use std::ptr;
use std::slice;

use crate::{bootstrap_origin, loopback_url};

const MAX_LINE: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Desktop,
    Probe,
    Isolation,
}

const MAIN_FRAME: u32 = 0;
const SUBFRAME: u32 = 1;
const RESOURCE: u32 = 2;

struct Scope {
    origin_prefix: String,
    blob_prefix: String,
    host_socket: Option<String>,
}

impl Scope {
    fn new(origin: &str, desktop: bool) -> Self {
        Self {
            origin_prefix: format!("{origin}/"),
            blob_prefix: format!("blob:{origin}/"),
            host_socket: desktop.then(|| format!("ws{}/_host/ws", &origin[4..])),
        }
    }

    fn allows(&self, operation: u32, url: &[u8]) -> bool {
        if url.starts_with(self.origin_prefix.as_bytes()) {
            return matches!(operation, MAIN_FRAME | SUBFRAME | RESOURCE);
        }
        match operation {
            SUBFRAME => url == b"about:blank",
            RESOURCE => url.starts_with(b"data:")
                || url.starts_with(self.blob_prefix.as_bytes())
                || self.host_socket.as_ref().is_some_and(|socket| url == socket.as_bytes()),
            _ => false,
        }
    }
}

struct Startup {
    mode: Mode,
    primary_url: CString,
    profile: CString,
    guest_url: CString,
    primary: Scope,
    guest: Option<Scope>,
}

fn valid_line(value: &str) -> bool {
    !value.is_empty()
        && value.len() < MAX_LINE
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

pub(super) fn valid_profile(profile: &str) -> bool {
    profile.starts_with('/') && valid_line(profile)
}

fn read_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = Vec::new();
    reader.take(MAX_LINE as u64).read_until(b'\n', &mut line).ok()?;
    if line.pop()? != b'\n' {
        return None;
    }
    String::from_utf8(line).ok()
}

impl Startup {
    fn new(mode: Mode, primary_url: &str, profile: &str, guest_url: &str) -> Option<Self> {
        if !valid_line(primary_url) || !valid_profile(profile) {
            return None;
        }
        let origin = if mode == Mode::Desktop {
            bootstrap_origin(primary_url, None)?
        } else {
            loopback_url(primary_url)?.1
        };
        let guest = if mode == Mode::Isolation {
            if !valid_line(guest_url) {
                return None;
            }
            let guest_origin = loopback_url(guest_url)?.1;
            if guest_origin == origin {
                return None;
            }
            Some(Scope::new(guest_origin, false))
        } else {
            None
        };
        Some(Self {
            mode,
            primary_url: CString::new(primary_url).ok()?,
            profile: CString::new(profile).ok()?,
            guest_url: CString::new(guest_url).ok()?,
            primary: Scope::new(origin, mode == Mode::Desktop),
            guest,
        })
    }

    fn read(mut reader: impl BufRead) -> Option<Self> {
        let mode = match read_line(&mut reader)?.as_str() {
            "desktop" => Mode::Desktop,
            "probe" => Mode::Probe,
            "isolation" => Mode::Isolation,
            _ => return None,
        };
        let primary = read_line(&mut reader)?;
        let profile = read_line(&mut reader)?;
        let guest = if mode == Mode::Isolation { read_line(&mut reader)? } else { String::new() };
        Self::new(mode, &primary, &profile, &guest)
    }

    fn allows(&self, guest: bool, operation: u32, url: &[u8]) -> bool {
        let scope = if guest { self.guest.as_ref() } else { Some(&self.primary) };
        scope.is_some_and(|scope| scope.allows(operation, url))
    }
}

#[repr(C)]
struct StartupView {
    desktop: bool,
    primary_url: *const c_char,
    profile: *const c_char,
    guest_url: *const c_char,
}

#[no_mangle]
unsafe extern "C" fn openshell_cef_startup(view: *mut StartupView) -> *mut Startup {
    let Some(startup) = Startup::read(io::stdin().lock()) else {
        return ptr::null_mut();
    };
    // The returned owner keeps these C strings alive until openshell_cef_free.
    unsafe { view.write(StartupView {
        desktop: startup.mode == Mode::Desktop,
        primary_url: startup.primary_url.as_ptr(),
        profile: startup.profile.as_ptr(),
        guest_url: startup.guest_url.as_ptr(),
    }) };
    Box::into_raw(Box::new(startup))
}

#[no_mangle]
unsafe extern "C" fn openshell_cef_free(startup: *mut Startup) {
    drop(unsafe { Box::from_raw(startup) });
}

#[no_mangle]
unsafe extern "C" fn openshell_cef_allows(
    startup: *const Startup,
    guest: bool,
    operation: u32,
    url: *const u8,
    length: usize,
) -> bool {
    unsafe { (*startup).allows(guest, operation, slice::from_raw_parts(url, length)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hostd_launch_url, write_cef_startup};

    const TOKEN: &str = "abcdefghijklmnopqrstuvwxyzABCDEF";

    #[test]
    fn launcher_and_cef_agree_on_the_bootstrap() {
        let valid = format!("OPENSHELL_HOSTD_READY http://127.0.0.1:7788/?token={TOKEN}");
        let url = hostd_launch_url(&valid, 7788).unwrap();
        let mut pipe = Vec::new();
        write_cef_startup(&mut pipe, url, "/private/profile").unwrap();
        let config = Startup::read(pipe.as_slice()).unwrap();
        assert!(config.allows(false, RESOURCE, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(false, MAIN_FRAME, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(true, RESOURCE, b"http://127.0.0.1:7788/"));
        assert!(hostd_launch_url(&valid, 7799).is_none());
        for invalid in [
            valid.replace("127.0.0.1", "localhost"),
            valid.replace("127.0.0.1", "127.0.0.1@192.0.2.1"),
            valid.replace("7788", "07788"),
            valid.replace("/?", "/api?"),
            format!("{valid}&other=1"),
            format!("{valid}#fragment"),
        ] {
            assert!(hostd_launch_url(&invalid, 7788).is_none());
        }
    }

    #[test]
    fn rejects_invalid_startup_lines_and_shared_origins() {
        for input in [
            "probe\nhttp://127.0.0.1:7788/\nrelative\n",
            "probe\nhttp://127.0.0.1:7788/\n/private\r\n",
            "probe\nhttp://127.0.0.1:7788/\n/private",
            "desktop\nhttp://127.0.0.1:7788/\n/private\n",
            "isolation\nhttp://127.0.0.1:7788/\n/private\nhttp://127.0.0.1:7788/guest\n",
        ] {
            assert!(Startup::read(input.as_bytes()).is_none());
        }
        assert!(Startup::read(vec![b'x'; MAX_LINE].as_slice()).is_none());
    }

    #[test]
    fn isolates_guest_navigation_and_resources() {
        let config = Startup::read(&b"isolation\nhttp://127.0.0.1:7788/\n/private\nhttp://127.0.0.1:7890/\n"[..]).unwrap();
        for operation in [MAIN_FRAME, SUBFRAME, RESOURCE] {
            assert!(config.allows(true, operation, b"http://127.0.0.1:7890/worker.js"));
            assert!(!config.allows(true, operation, b"http://127.0.0.1:7788/private"));
            assert!(!config.allows(true, operation, b"http://127.0.0.1:7890@192.0.2.1/"));
        }
        assert!(!config.allows(true, RESOURCE, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(true, RESOURCE, b"blob:http://127.0.0.1:7788/id"));
        assert!(config.allows(true, RESOURCE, b"blob:http://127.0.0.1:7890/id"));
        assert!(config.allows(true, SUBFRAME, b"about:blank"));
        assert!(!config.allows(true, MAIN_FRAME, b"about:blank"));
    }
}
