pub mod authproto;
mod cef;
pub mod install;
pub mod login;
pub mod power;
pub mod settings;
pub mod update;
pub mod session;

use std::io::{self, BufRead, Read, Write};

const LOOPBACK: &str = "http://127.0.0.1:";
const READY: &str = "OPENSHELL_HOSTD_READY ";

fn loopback_url(url: &str) -> Option<(u16, &str, &str)> {
    let address = url.strip_prefix(LOOPBACK)?;
    let (port, path) = address.split_once('/')?;
    let number = port.parse::<u16>().ok()?;
    if number == 0 || port != number.to_string() {
        return None;
    }
    Some((number, &url[..LOOPBACK.len() + port.len()], path))
}

fn bootstrap_origin(url: &str, expected_port: Option<u16>) -> Option<&str> {
    let (port, origin, path) = loopback_url(url)?;
    let token = path.strip_prefix("?token=")?;
    if expected_port.is_some_and(|expected| expected != port)
        || !(32..=128).contains(&token.len())
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return None;
    }
    Some(origin)
}

pub fn hostd_launch_url(line: &str, expected_port: u16) -> Option<&str> {
    let url = line.strip_prefix(READY)?;
    bootstrap_origin(url, Some(expected_port))?;
    Some(url)
}

pub fn is_hostd_readiness(line: &str) -> bool {
    line.starts_with(READY)
}

pub fn read_hostd_readiness(reader: &mut impl BufRead, port: u16) -> io::Result<String> {
    const MAX_LINE: usize = 4096;
    let mut line = Vec::new();
    let mut discard = false;
    loop {
        line.clear();
        let length = Read::take(&mut *reader, (MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if length == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "hostd closed stdout before readiness",
            ));
        }
        if !line.ends_with(b"\n") {
            discard = true;
            continue;
        }
        if std::mem::take(&mut discard) || length > MAX_LINE {
            continue;
        }
        let Ok(value) = std::str::from_utf8(&line) else {
            continue;
        };
        if is_hostd_readiness(value) {
            return hostd_launch_url(value.trim_end_matches(['\r', '\n']), port)
                .map(str::to_owned)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "hostd emitted an invalid readiness URL")
                });
        }
    }
}

pub fn write_cef_startup(mut pipe: impl Write, url: &str, profile: &str) -> io::Result<()> {
    if bootstrap_origin(url, None).is_none() || !cef::valid_profile(profile) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid CEF startup configuration",
        ));
    }
    writeln!(pipe, "desktop\n{url}\n{profile}")?;
    pipe.flush()
}

/// Tells a running Desktop host to start over with a restarted hostd.
pub fn write_cef_bootstrap(mut pipe: impl Write, url: &str) -> io::Result<()> {
    if bootstrap_origin(url, None).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid hostd bootstrap URL",
        ));
    }
    writeln!(pipe, "{}{url}", cef::BOOTSTRAP)?;
    pipe.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_readiness_prefix_inside_an_oversized_log_line() {
        let valid = "http://127.0.0.1:7788/?token=abcdefghijklmnopqrstuvwxyzABCDEF";
        let input = format!("{}{READY}not-a-url\n{READY}{valid}\r\n", "x".repeat(4097));
        assert_eq!(read_hostd_readiness(&mut input.as_bytes(), 7788).unwrap(), valid);
        assert!(read_hostd_readiness(&mut input.as_bytes(), 7799).is_err());
    }
}
