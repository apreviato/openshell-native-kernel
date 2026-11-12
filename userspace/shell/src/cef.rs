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

/// The browsing contexts of the host, as the C++ adapter names them.
const PRIMARY: u32 = 0;
const GUEST: u32 = 1;
const BROWSER: u32 = 2;

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

/// The scheme, host and port of an http(s) or ws(s) URL as Chromium
/// canonicalizes it: lowercase host, dotted IPv4, bracketed IPv6 and no
/// default port.
fn web_target(url: &str) -> Option<(&str, &str, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let address = authority.rsplit_once('@').map_or(authority, |(_, address)| address);
    let (host, port) = if address.starts_with('[') {
        let end = address.find(']')? + 1;
        match &address[end..] {
            "" => (&address[..end], default),
            port => (&address[..end], port.strip_prefix(':')?.parse().ok()?),
        }
    } else {
        match address.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (address, default),
        }
    };
    (!host.is_empty()).then_some((scheme, host, port))
}

/// Host names that reach this computer without DNS.
fn loopback_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.starts_with("127.")
        || host == "0.0.0.0"
        || matches!(host, "[::1]" | "[::]" | "[::ffff:0:0]")
        || host.starts_with("[::ffff:7f")
}

/// The Browser's contexts reach the web and the user's local services, but not
/// hostd, which serves the Desktop and its privileged API on a loopback port.
struct BrowserScope {
    hostd_port: u16,
}

impl BrowserScope {
    fn web(&self, url: &str, sockets: bool) -> bool {
        let Some((scheme, host, port)) = web_target(url) else { return false };
        (sockets || !scheme.starts_with("ws")) && !(port == self.hostd_port && loopback_host(host))
    }

    fn allows(&self, operation: u32, url: &[u8]) -> bool {
        let Ok(url) = std::str::from_utf8(url) else { return false };
        match operation {
            MAIN_FRAME => url == "about:blank" || self.web(url, false),
            SUBFRAME => matches!(url, "about:blank" | "about:srcdoc")
                || url.starts_with("data:")
                || url.starts_with("blob:")
                || self.web(url, false),
            RESOURCE => url.starts_with("data:") || url.starts_with("blob:") || self.web(url, true),
            _ => false,
        }
    }
}

/// Whether a Desktop request opens or navigates a Browser tab to this URL.
pub(super) fn browser_url(url: &str, hostd_port: u16) -> bool {
    url.len() < MAX_LINE && BrowserScope { hostd_port }.allows(MAIN_FRAME, url.as_bytes())
}

struct Startup {
    mode: Mode,
    origin: String,
    primary_url: CString,
    profile: CString,
    guest_url: CString,
    primary: Scope,
    guest: Option<Scope>,
    browser: Option<BrowserScope>,
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
        let browser = match mode {
            Mode::Desktop => Some(BrowserScope { hostd_port: origin.rsplit_once(':')?.1.parse().ok()? }),
            _ => None,
        };
        Some(Self {
            browser,
            mode,
            origin: origin.to_owned(),
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

    fn allows(&self, context: u32, operation: u32, url: &[u8]) -> bool {
        match context {
            PRIMARY => self.primary.allows(operation, url),
            GUEST => self.guest.as_ref().is_some_and(|scope| scope.allows(operation, url)),
            BROWSER => self.browser.as_ref().is_some_and(|scope| scope.allows(operation, url)),
            _ => false,
        }
    }
}

/// The launcher's messages to a running Desktop host: one line per hostd
/// restart, with the new bootstrap URL on the original origin.
pub(super) const BOOTSTRAP: &str = "bootstrap ";

fn control_url<'a>(line: &'a str, origin: &str) -> Option<&'a str> {
    let url = line.strip_prefix(BOOTSTRAP)?;
    (bootstrap_origin(url, None)? == origin).then_some(url)
}

/// Owned by the host's control thread, independently of the startup object.
struct Control {
    origin: String,
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
unsafe extern "C" fn openshell_cef_control(startup: *const Startup) -> *mut Control {
    let startup = unsafe { &*startup };
    if startup.mode != Mode::Desktop {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Control { origin: startup.origin.clone() }))
}

/// Blocks until the launcher sends a valid bootstrap URL, copies it with a
/// trailing NUL and returns its length. Returns -1 when the launcher closed
/// the pipe or sent anything else: the Desktop session then ends.
#[no_mangle]
unsafe extern "C" fn openshell_cef_control_next(
    control: *const Control,
    buffer: *mut c_char,
    capacity: usize,
) -> isize {
    let control = unsafe { &*control };
    let Some(line) = read_line(&mut io::stdin().lock()) else { return -1 };
    match control_url(&line, &control.origin) {
        Some(url) if url.len() < capacity => {
            unsafe {
                ptr::copy_nonoverlapping(url.as_ptr(), buffer.cast(), url.len());
                buffer.add(url.len()).write(0);
            }
            url.len() as isize
        }
        _ => -1,
    }
}

#[no_mangle]
unsafe extern "C" fn openshell_cef_control_free(control: *mut Control) {
    drop(unsafe { Box::from_raw(control) });
}

#[no_mangle]
unsafe extern "C" fn openshell_cef_allows(
    startup: *const Startup,
    context: u32,
    operation: u32,
    url: *const u8,
    length: usize,
) -> bool {
    unsafe { (*startup).allows(context, operation, slice::from_raw_parts(url, length)) }
}

/// The hostd port of a Desktop session, which the Browser may not reach.
#[no_mangle]
unsafe extern "C" fn openshell_cef_hostd_port(startup: *const Startup) -> u16 {
    unsafe { (*startup).browser.as_ref().map_or(0, |scope| scope.hostd_port) }
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
        assert!(config.allows(PRIMARY, RESOURCE, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(PRIMARY, MAIN_FRAME, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(GUEST, RESOURCE, b"http://127.0.0.1:7788/"));
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
    fn accepts_new_bootstraps_only_on_the_desktop_origin() {
        let first = format!("http://127.0.0.1:7788/?token={TOKEN}");
        let second = format!("http://127.0.0.1:7788/?token={}", TOKEN.to_lowercase());
        let mut pipe = Vec::new();
        write_cef_startup(&mut pipe, &first, "/private/profile").unwrap();
        crate::write_cef_bootstrap(&mut pipe, &second).unwrap();
        let mut reader = pipe.as_slice();
        let config = Startup::read(&mut reader).unwrap();
        let line = read_line(&mut reader).unwrap();
        assert_eq!(control_url(&line, &config.origin), Some(second.as_str()));
        for invalid in [
            format!("bootstrap {}", second.replace("7788", "7789")),
            format!("bootstrap http://127.0.0.1:7788/"),
            format!("navigate {second}"),
            second.clone(),
        ] {
            assert!(control_url(&invalid, &config.origin).is_none());
        }
        assert!(crate::write_cef_bootstrap(Vec::new(), "http://127.0.0.1:7788/").is_err());
    }

    #[test]
    fn keeps_the_browser_away_from_hostd() {
        let url = format!("http://127.0.0.1:7788/?token={TOKEN}");
        let mut pipe = Vec::new();
        write_cef_startup(&mut pipe, &url, "/private/profile").unwrap();
        let config = Startup::read(pipe.as_slice()).unwrap();
        for allowed in [
            "https://example.com/",
            "http://example.com:7788/",
            "http://127.0.0.1:8080/app",
            "http://localhost:3000/",
            "about:blank",
        ] {
            assert!(config.allows(BROWSER, MAIN_FRAME, allowed.as_bytes()), "{allowed}");
        }
        for denied in [
            "http://127.0.0.1:7788/",
            "http://127.0.0.2:7788/x",
            "http://localhost:7788/",
            "http://app.localhost:7788/",
            "http://0.0.0.0:7788/",
            "http://[::1]:7788/",
            "http://[::ffff:7f00:1]:7788/",
            "http://user@127.0.0.1:7788/",
            "file:///etc/passwd",
            "chrome://settings/",
            "javascript:alert(1)",
            "ws://example.com/",
        ] {
            assert!(!config.allows(BROWSER, MAIN_FRAME, denied.as_bytes()), "{denied}");
        }
        assert!(config.allows(BROWSER, RESOURCE, b"wss://example.com/socket"));
        assert!(!config.allows(BROWSER, RESOURCE, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(config.allows(BROWSER, SUBFRAME, b"about:srcdoc"));
        assert!(!config.allows(PRIMARY, MAIN_FRAME, b"https://example.com/"));
        assert!(browser_url("https://example.com/", 7788));
        assert!(!browser_url("http://localhost:7788/", 7788));
        let probe = Startup::read(&b"probe\nhttp://127.0.0.1:7788/\n/private\n"[..]).unwrap();
        assert!(!probe.allows(BROWSER, MAIN_FRAME, b"https://example.com/"));
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
            assert!(config.allows(GUEST, operation, b"http://127.0.0.1:7890/worker.js"));
            assert!(!config.allows(GUEST, operation, b"http://127.0.0.1:7788/private"));
            assert!(!config.allows(GUEST, operation, b"http://127.0.0.1:7890@192.0.2.1/"));
        }
        assert!(!config.allows(GUEST, RESOURCE, b"ws://127.0.0.1:7788/_host/ws"));
        assert!(!config.allows(GUEST, RESOURCE, b"blob:http://127.0.0.1:7788/id"));
        assert!(config.allows(GUEST, RESOURCE, b"blob:http://127.0.0.1:7890/id"));
        assert!(config.allows(GUEST, SUBFRAME, b"about:blank"));
        assert!(!config.allows(GUEST, MAIN_FRAME, b"about:blank"));
    }
}
