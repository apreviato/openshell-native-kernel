//! XSecureLock's authentication protocol, the helper's side
//! (https://github.com/google/xsecurelock/blob/master/helpers/authproto.h).
//!
//! The lock screen (userspace/lock, an XSecureLock auth module) runs a helper
//! and exchanges packets with it, `<type> <length>\n<message>\n` each: the
//! helper asks with a prompt ('P' hidden, 'U' visible), the lock screen
//! answers ('p', 'u') or cancels ('x'), and the helper may say why it failed
//! ('e') or anything else ('i'). Only the helper's exit status unlocks.
//! openshell-authproto is that helper on OpenShell Native: it asks the
//! session supervisor, which holds the password's hash (login.rs).

use std::io::{self, BufRead, Write};

/// Longer answers are refused before reading them.
pub const MAX_MESSAGE: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub message: Vec<u8>,
}

impl Drop for Packet {
    // Answers hold passwords: none stays in freed memory.
    fn drop(&mut self) {
        for byte in self.message.iter_mut() {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

pub fn write_packet(mut output: impl Write, kind: u8, message: &str) -> io::Result<()> {
    write!(output, "{} {}\n{message}\n", kind as char, message.len())?;
    output.flush()
}

/// One packet, or an error for anything malformed or too long.
pub fn read_packet(input: &mut impl BufRead) -> io::Result<Packet> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut header = Vec::new();
    io::Read::take(&mut *input, 16).read_until(b'\n', &mut header)?;
    let header = header.strip_suffix(b"\n").ok_or_else(|| invalid("unterminated packet header"))?;
    let [kind, b' ', length @ ..] = header else { return Err(invalid("invalid packet header")) };
    if !kind.is_ascii_alphabetic() || length.is_empty() || !length.iter().all(u8::is_ascii_digit) {
        return Err(invalid("invalid packet header"));
    }
    let length: usize = std::str::from_utf8(length).ok().and_then(|text| text.parse().ok()).ok_or_else(|| invalid("invalid packet length"))?;
    if length > MAX_MESSAGE {
        return Err(invalid("packet too long"));
    }
    let mut packet = Packet { kind: *kind, message: vec![0; length + 1] };
    io::Read::read_exact(input, &mut packet.message)?;
    if packet.message.pop() != Some(b'\n') {
        return Err(invalid("unterminated packet"));
    }
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_packets() {
        let mut output = Vec::new();
        write_packet(&mut output, b'P', "Password:").unwrap();
        assert_eq!(output, b"P 9\nPassword:\n");
        let mut input: &[u8] = b"p 10\ntwo words!\nx 0\n\n";
        assert_eq!(read_packet(&mut input).unwrap(), Packet { kind: b'p', message: b"two words!".to_vec() });
        assert_eq!(read_packet(&mut input).unwrap(), Packet { kind: b'x', message: Vec::new() });
        for bad in [&b"p 3\nabcd"[..], b"p3\nabc\n", b"p -1\n\n", b"p 99999\n", b"pp 1\na\n", b"p 1\nab"] {
            assert!(read_packet(&mut &bad[..]).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
