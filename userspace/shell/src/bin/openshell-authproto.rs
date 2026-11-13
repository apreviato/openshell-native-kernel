//! The lock screen's authentication helper on OpenShell Native (authproto.rs):
//! it asks for the password and has the session supervisor check it on its
//! system socket (login.rs, Phase::Unlock). It exits 0 only for "ok".

use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use openshell_shell::authproto::{read_packet, write_packet};
use openshell_shell::install::SYSTEM_SOCKET_ENV;
use openshell_shell::login::valid_password;

const PROMPT: &str = "Password:";
/// The supervisor waits a moment after a wrong password.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

fn ask_supervisor(socket: &str, password: &[u8]) -> io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(ANSWER_TIMEOUT))?;
    let mut request = Vec::with_capacity(password.len() + 7);
    request.extend_from_slice(b"login ");
    request.extend_from_slice(password);
    request.push(b'\n');
    let written = stream.write_all(&request);
    request.iter_mut().for_each(|byte| unsafe { std::ptr::write_volatile(byte, 0) });
    written?;
    let mut answer = String::new();
    BufReader::new(stream).take(256).read_line(&mut answer)?;
    Ok(answer.trim_end().to_owned())
}

fn run() -> io::Result<bool> {
    let mut output = io::stdout().lock();
    let Some(socket) = env::var_os(SYSTEM_SOCKET_ENV).and_then(|value| value.into_string().ok()) else {
        write_packet(&mut output, b'e', "This session has no sign-in to unlock with.")?;
        return Ok(false);
    };
    write_packet(&mut output, b'P', PROMPT)?;
    let packet = read_packet(&mut io::stdin().lock())?;
    if packet.kind != b'p' {
        return Ok(false);
    }
    if !std::str::from_utf8(&packet.message).is_ok_and(valid_password) {
        write_packet(&mut output, b'e', "Incorrect password.")?;
        return Ok(false);
    }
    let answer = ask_supervisor(&socket, &packet.message);
    match answer.as_deref() {
        Ok("ok") => Ok(true),
        Ok("wrong") => {
            write_packet(&mut output, b'e', "Incorrect password.")?;
            Ok(false)
        }
        _ => {
            write_packet(&mut output, b'e', "The password could not be checked.")?;
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("openshell-authproto: {error}");
            ExitCode::FAILURE
        }
    }
}
