use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();
    let exit = vaco::probe::run(&argv, &mut out, &mut err);
    if let Err(error) = out.flush() {
        if error.kind() != std::io::ErrorKind::BrokenPipe {
            let _ = writeln!(err, "{error}");
            return ExitCode::FAILURE;
        }
    }
    match exit {
        vaco::probe::Exit::Ok => ExitCode::SUCCESS,
        vaco::probe::Exit::Failure => ExitCode::FAILURE,
    }
}
