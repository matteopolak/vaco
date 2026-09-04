#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use vaco_vecheck::{Config, Date, VecheckError, cargo_asm, verify_assembly, verify_remarks};

const USAGE: &str = "Usage:
  vaco-vecheck remarks --config <vecheck.toml> --remarks <remarks.yaml> [--today YYYY-MM-DD]
  vaco-vecheck asm --config <vecheck.toml> --target <tier> --kernel <id> [--asm <assembly.s>] [--today YYYY-MM-DD]
  vaco-vecheck validate --config <vecheck.toml> [--today YYYY-MM-DD]";

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vaco-vecheck: {error}");
            ExitCode::from(1)
        }
    }
}

fn run(arguments: &[String]) -> Result<(), VecheckError> {
    let Some((command, rest)) = arguments.split_first() else {
        return Err(VecheckError::usage(USAGE));
    };
    let arguments = Arguments::parse(rest)?;
    let config_path = arguments.required("--config")?;
    let config = read_config(config_path)?;
    let today = match arguments.optional("--today") {
        Some(value) => Date::parse(value)?,
        None => Date::today()?,
    };
    config.validate(today)?;

    match command.as_str() {
        "remarks" => {
            arguments.reject_unknown(&["--config", "--remarks", "--today"])?;
            let remarks = read_text(arguments.required("--remarks")?)?;
            verify_remarks(&config, &remarks)?;
            for kernel in config.kernels() {
                println!("{}: vectorized", kernel.id);
            }
        }
        "asm" => {
            arguments.reject_unknown(&["--config", "--target", "--kernel", "--asm", "--today"])?;
            let target = arguments.required("--target")?;
            let kernel_id = arguments.required("--kernel")?;
            let assembly = if let Some(path) = arguments.optional("--asm") {
                read_text(path)?
            } else {
                let kernel = config.kernel(kernel_id).ok_or_else(|| {
                    VecheckError::usage(format!("unknown kernel '{kernel_id}'\n{USAGE}"))
                })?;
                cargo_asm(kernel)?
            };
            verify_assembly(&config, target, kernel_id, &assembly)?;
            println!("{kernel_id} ({target}): assembly contract satisfied");
        }
        "validate" => {
            arguments.reject_unknown(&["--config", "--today"])?;
            println!(
                "vecheck.toml: {} kernel contracts valid",
                config.kernels().len()
            );
        }
        _ => return Err(VecheckError::usage(USAGE)),
    }
    Ok(())
}

fn read_config(path: &str) -> Result<Config, VecheckError> {
    Config::parse(&read_text(path)?)
}

fn read_text(path: &str) -> Result<String, VecheckError> {
    fs::read_to_string(Path::new(path))
        .map_err(|error| VecheckError::usage(format!("could not read {path}: {error}")))
}

#[derive(Debug)]
struct Arguments {
    entries: Vec<(String, String)>,
}

impl Arguments {
    fn parse(arguments: &[String]) -> Result<Self, VecheckError> {
        let mut entries = Vec::new();
        let mut arguments = arguments.iter();
        while let Some(key) = arguments.next() {
            if !key.starts_with("--") {
                return Err(VecheckError::usage(format!(
                    "expected option, got '{key}'\n{USAGE}"
                )));
            }
            let Some(value) = arguments.next() else {
                return Err(VecheckError::usage(format!(
                    "missing value for {key}\n{USAGE}"
                )));
            };
            if value.starts_with("--") {
                return Err(VecheckError::usage(format!(
                    "missing value for {key}\n{USAGE}"
                )));
            }
            if entries.iter().any(|(existing, _)| existing == key) {
                return Err(VecheckError::usage(format!(
                    "duplicate option {key}\n{USAGE}"
                )));
            }
            entries.push((key.clone(), value.clone()));
        }
        Ok(Self { entries })
    }

    fn required(&self, key: &str) -> Result<&str, VecheckError> {
        self.optional(key)
            .ok_or_else(|| VecheckError::usage(format!("missing required {key}\n{USAGE}")))
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|(entry, value)| (entry == key).then_some(value.as_str()))
    }

    fn reject_unknown(&self, accepted: &[&str]) -> Result<(), VecheckError> {
        if let Some((key, _)) = self
            .entries
            .iter()
            .find(|(key, _)| !accepted.contains(&key.as_str()))
        {
            return Err(VecheckError::usage(format!(
                "unknown option {key}\n{USAGE}"
            )));
        }
        Ok(())
    }
}
