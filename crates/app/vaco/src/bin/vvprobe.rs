use std::io::Write as _;
use std::process::ExitCode;

const LEGACY_BANNER: &str = concat!(
    "vaco-probe version ",
    env!("CARGO_PKG_VERSION"),
    " Copyright (c) 2026 the Vaco authors"
);
const INSTALLED_BANNER: &str = concat!(
    "vvprobe version ",
    env!("CARGO_PKG_VERSION"),
    " Copyright (c) 2026 the Vaco authors"
);
const LEGACY_VERSION: &str = concat!("vaco-probe version ", env!("CARGO_PKG_VERSION"));
const INSTALLED_VERSION: &str = concat!("vvprobe version ", env!("CARGO_PKG_VERSION"));
const LEGACY_LICENSE: &str = "vaco-probe is licensed under GPL-3.0-or-later.";
const INSTALLED_LICENSE: &str = "vvprobe is licensed under GPL-3.0-or-later.";
const LEGACY_USAGE: &str = "usage: vaco-probe [OPTIONS] INPUT_FILE";
const INSTALLED_USAGE: &str = "usage: vvprobe [OPTIONS] INPUT_FILE";

fn branding() -> [(&'static [u8], &'static [u8]); 4] {
    [
        (LEGACY_BANNER.as_bytes(), INSTALLED_BANNER.as_bytes()),
        (LEGACY_VERSION.as_bytes(), INSTALLED_VERSION.as_bytes()),
        (LEGACY_LICENSE.as_bytes(), INSTALLED_LICENSE.as_bytes()),
        (LEGACY_USAGE.as_bytes(), INSTALLED_USAGE.as_bytes()),
    ]
}

fn main() -> ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = vaco::probe::run(&argv, &mut out, &mut err);
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = std::io::BufWriter::new(stdout.lock());
    let mut stderr = stderr.lock();
    let branding = branding();
    if let Err(error) = stdout.write_all(&vaco::command::rebrand_static_lines(&out, &branding))
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        let _ = writeln!(stderr, "{error}");
        return ExitCode::FAILURE;
    }
    let _ = stderr.write_all(&vaco::command::rebrand_static_lines(&err, &branding));
    if let Err(error) = stdout.flush()
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        let _ = writeln!(stderr, "{error}");
        return ExitCode::FAILURE;
    }
    match exit {
        vaco::probe::Exit::Ok => ExitCode::SUCCESS,
        vaco::probe::Exit::Failure => ExitCode::FAILURE,
    }
}
