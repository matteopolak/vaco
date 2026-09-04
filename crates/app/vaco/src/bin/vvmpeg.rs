use std::io::Write;

fn main() {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();
    let code = vaco::cli::run(&argv, &mut out, &mut err);
    let _ = out.flush();
    std::process::exit(code.code());
}
