//! Powers off or restarts the computer from the session, through the session
//! supervisor (power.rs): `openshell-power poweroff|reboot`. The lock screen's
//! buttons run it; it exits 0 once the supervisor has taken the request.

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::time::Duration;

use openshell_shell::install::SYSTEM_SOCKET_ENV;
use openshell_shell::power::{PowerAction, POWER_REQUEST};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let request = format!("{POWER_REQUEST} {}", args.first().map(String::as_str).unwrap_or(""));
    if args.len() != 1 || PowerAction::from_request(&request).is_none() {
        eprintln!("Usage: openshell-power poweroff|reboot");
        return ExitCode::from(2);
    }
    let Some(socket) = env::var_os(SYSTEM_SOCKET_ENV) else {
        eprintln!("openshell-power: this session has no supervisor to ask");
        return ExitCode::FAILURE;
    };
    let answer = (|| -> std::io::Result<String> {
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        writeln!(stream, "{request}")?;
        let mut answer = String::new();
        BufReader::new(stream).take(256).read_line(&mut answer)?;
        Ok(answer.trim_end().to_owned())
    })();
    match answer.as_deref() {
        Ok("ok") => ExitCode::SUCCESS,
        Ok(other) => {
            eprintln!("openshell-power: the supervisor answered {other:?}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("openshell-power: {error}");
            ExitCode::FAILURE
        }
    }
}
