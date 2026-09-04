#![forbid(unsafe_code)]
//! Vectorization-contract parsing and assembly assertions.
//!
//! `vaco-vecheck` deliberately has no dependency on a TOML or regular-expression
//! crate. Its input grammar is the small, documented `vecheck.toml` subset and
//! its instruction patterns are literal alternatives with optional `\\b` word
//! boundaries. Keeping that surface small makes the merge gate inspectable and
//! avoids a tool dependency entering every developer workflow.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

/// A calendar date used to make waiver expiry deterministic in tests and CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// Parse an ISO-8601 calendar date (`YYYY-MM-DD`).
    ///
    /// # Errors
    ///
    /// Returns [`VecheckError`] when the date is malformed or impossible.
    pub fn parse(value: &str) -> Result<Self, VecheckError> {
        let mut parts = value.split('-');
        let year: i32 = parse_part(parts.next(), "year", value)?;
        let month: u8 = parse_part(parts.next(), "month", value)?;
        let day: u8 = parse_part(parts.next(), "day", value)?;
        if parts.next().is_some() || !(1..=12).contains(&month) {
            return Err(VecheckError::new(format!("invalid date '{value}'")));
        }
        let days = days_in_month(year, month);
        if day == 0 || day > days {
            return Err(VecheckError::new(format!("invalid date '{value}'")));
        }
        Ok(Self { year, month, day })
    }

    /// Read today's UTC calendar date from the system clock.
    ///
    /// # Errors
    ///
    /// Returns [`VecheckError`] if the system clock predates Unix epoch.
    pub fn today() -> Result<Self, VecheckError> {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                VecheckError::new(format!("system clock before Unix epoch: {error}"))
            })?;
        let days = i64::try_from(elapsed.as_secs().div_euclid(86_400))
            .map_err(|_| VecheckError::new("system clock is outside supported date range"))?;
        Ok(civil_from_days(days))
    }
}

impl fmt::Display for Date {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

/// A parsed `vecheck.toml` contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    max_live_waiver_cost_pct: f64,
    kernels: Vec<Kernel>,
    waivers: Vec<Waiver>,
}

/// One kernel's declared symbol and target-specific assembly expectations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    /// Stable config identifier.
    pub id: String,
    /// Kernel-slot variant governed by this contract.
    pub variant: String,
    /// Rust symbol substring used for LLVM remark matching and `cargo asm`.
    pub symbol: String,
    /// Optional emitted-item selector when the source body is inlined.
    pub asm_symbol: Option<String>,
    /// Cargo package that owns the symbol.
    pub package: String,
    /// Optional Rust target triple for `cargo asm`.
    pub cargo_target: Option<String>,
    /// Optional target CPU passed to `cargo-show-asm`.
    pub cargo_target_cpu: Option<String>,
    /// Optional artifact selector such as `--lib` for `cargo-show-asm`.
    pub cargo_artifact: Option<String>,
    expectations: BTreeMap<String, AssemblyExpectation>,
}

/// Required and forbidden instruction patterns for one ISA tier.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssemblyExpectation {
    /// Every pattern must occur in the assembly text.
    pub require: Vec<String>,
    /// No pattern may occur in the assembly text.
    pub forbid: Vec<String>,
    /// Optional instruction-count ceiling.
    pub max_insns: Option<usize>,
}

/// A temporary, measured exception to a vectorization contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiver {
    /// Config kernel id.
    pub kernel: String,
    /// Affected variant label.
    pub variant: String,
    /// Human-readable reason.
    pub reason: String,
    /// Upstream tracking URL.
    pub upstream: String,
    /// First date on which CI must reject the waiver.
    pub expires: Date,
    /// Measured performance cost included in the live-waiver total.
    pub cost_pct: f64,
}

impl Config {
    /// Parse the documented, deliberately narrow `vecheck.toml` grammar.
    ///
    /// # Errors
    ///
    /// Returns [`VecheckError`] for unknown sections, malformed values, or an
    /// incomplete kernel/waiver declaration.
    pub fn parse(source: &str) -> Result<Self, VecheckError> {
        let mut config = Self {
            max_live_waiver_cost_pct: 3.0,
            kernels: Vec::new(),
            waivers: Vec::new(),
        };
        let mut section = Section::Root;

        for (number, raw) in source.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line == "[[kernel]]" {
                config.kernels.push(Kernel {
                    id: String::new(),
                    variant: String::new(),
                    symbol: String::new(),
                    asm_symbol: None,
                    package: String::new(),
                    cargo_target: None,
                    cargo_target_cpu: None,
                    cargo_artifact: None,
                    expectations: BTreeMap::new(),
                });
                section = Section::Kernel(config.kernels.len() - 1);
                continue;
            }
            if line == "[[waiver]]" {
                config.waivers.push(Waiver {
                    kernel: String::new(),
                    variant: String::new(),
                    reason: String::new(),
                    upstream: String::new(),
                    expires: Date {
                        year: 0,
                        month: 1,
                        day: 1,
                    },
                    cost_pct: 0.0,
                });
                section = Section::Waiver(config.waivers.len() - 1);
                continue;
            }
            if let Some(target) = line
                .strip_prefix("[kernel.expect.")
                .and_then(|rest| rest.strip_suffix(']'))
            {
                let (Section::Kernel(index) | Section::Expect(index, _)) = section else {
                    return Err(line_error(
                        number,
                        "kernel expectation needs a preceding [[kernel]]",
                    ));
                };
                let target = section_name(target)?;
                let kernel = config
                    .kernels
                    .get_mut(index)
                    .ok_or_else(|| line_error(number, "missing preceding [[kernel]]"))?;
                kernel.expectations.entry(target.clone()).or_default();
                section = Section::Expect(index, target);
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(line_error(number, "expected key = value"));
            };
            let key = key.trim();
            let value = value.trim();
            match &section {
                Section::Root if key == "max_live_waiver_cost_pct" => {
                    config.max_live_waiver_cost_pct = parse_number(value, number)?;
                }
                Section::Kernel(index) => {
                    let kernel = config
                        .kernels
                        .get_mut(*index)
                        .ok_or_else(|| line_error(number, "missing preceding [[kernel]]"))?;
                    set_kernel(kernel, key, value, number)?;
                }
                Section::Expect(index, target) => {
                    let kernel = config
                        .kernels
                        .get_mut(*index)
                        .ok_or_else(|| line_error(number, "missing preceding [[kernel]]"))?;
                    let expectation = kernel
                        .expectations
                        .get_mut(target)
                        .ok_or_else(|| line_error(number, "missing kernel expectation"))?;
                    set_expectation(expectation, key, value, number)?;
                }
                Section::Waiver(index) => {
                    let waiver = config
                        .waivers
                        .get_mut(*index)
                        .ok_or_else(|| line_error(number, "missing preceding [[waiver]]"))?;
                    set_waiver(waiver, key, value, number)?;
                }
                Section::Root => {
                    return Err(line_error(number, format!("unknown root key '{key}'")));
                }
            }
        }
        config.validate_shape()?;
        Ok(config)
    }

    /// Reject expired waivers and unbounded aggregate performance debt.
    ///
    /// # Errors
    ///
    /// Returns [`VecheckError`] when a waiver has expired or live cost exceeds
    /// `max_live_waiver_cost_pct`.
    pub fn validate(&self, today: Date) -> Result<(), VecheckError> {
        for waiver in &self.waivers {
            if waiver.expires < today {
                return Err(VecheckError::new(format!(
                    "waiver for {} / {} expired on {}",
                    waiver.kernel, waiver.variant, waiver.expires
                )));
            }
        }
        let total: f64 = self.waivers.iter().map(|waiver| waiver.cost_pct).sum();
        if total > self.max_live_waiver_cost_pct {
            return Err(VecheckError::new(format!(
                "live waiver cost {total:.3}% exceeds {:.3}%",
                self.max_live_waiver_cost_pct
            )));
        }
        Ok(())
    }

    /// Look up one declared kernel by its stable id.
    #[must_use]
    pub fn kernel(&self, id: &str) -> Option<&Kernel> {
        self.kernels.iter().find(|kernel| kernel.id == id)
    }

    /// All configured kernel contracts in declaration order.
    #[must_use]
    pub fn kernels(&self) -> &[Kernel] {
        &self.kernels
    }

    fn validate_shape(&self) -> Result<(), VecheckError> {
        let mut declared = BTreeSet::new();
        for kernel in &self.kernels {
            if kernel.id.is_empty()
                || kernel.variant.is_empty()
                || kernel.symbol.is_empty()
                || kernel.package.is_empty()
            {
                return Err(VecheckError::new(
                    "each [[kernel]] needs id, variant, symbol and package",
                ));
            }
            if !declared.insert((&kernel.id, &kernel.variant)) {
                return Err(VecheckError::new(format!(
                    "duplicate kernel contract {} / {}",
                    kernel.id, kernel.variant
                )));
            }
        }
        for waiver in &self.waivers {
            if waiver.kernel.is_empty()
                || waiver.variant.is_empty()
                || waiver.reason.is_empty()
                || waiver.upstream.is_empty()
            {
                return Err(VecheckError::new(
                    "each [[waiver]] needs kernel, variant, reason, upstream, expires and cost_pct",
                ));
            }
            if !declared.contains(&(&waiver.kernel, &waiver.variant)) {
                return Err(VecheckError::new(format!(
                    "waiver references undeclared kernel {} / {}",
                    waiver.kernel, waiver.variant
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Section {
    Root,
    Kernel(usize),
    Expect(usize, String),
    Waiver(usize),
}

fn set_kernel(
    kernel: &mut Kernel,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), VecheckError> {
    match key {
        "id" => kernel.id = unquote(value)?,
        "variant" => kernel.variant = unquote(value)?,
        "symbol" => kernel.symbol = unquote(value)?,
        "asm_symbol" => kernel.asm_symbol = Some(unquote(value)?),
        "package" => kernel.package = unquote(value)?,
        "cargo_target" => kernel.cargo_target = Some(unquote(value)?),
        "cargo_target_cpu" => kernel.cargo_target_cpu = Some(unquote(value)?),
        "cargo_artifact" => kernel.cargo_artifact = Some(unquote(value)?),
        _ => return Err(line_error(line, format!("unknown kernel key '{key}'"))),
    }
    Ok(())
}

fn set_expectation(
    expectation: &mut AssemblyExpectation,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), VecheckError> {
    match key {
        "require" => expectation.require = parse_string_array(value)?,
        "forbid" => expectation.forbid = parse_string_array(value)?,
        "max_insns" => {
            expectation.max_insns = Some(value.parse::<usize>().map_err(|_| {
                line_error(
                    line,
                    format!("max_insns must be a non-negative integer, got '{value}'"),
                )
            })?);
        }
        _ => return Err(line_error(line, format!("unknown expectation key '{key}'"))),
    }
    Ok(())
}

fn set_waiver(
    waiver: &mut Waiver,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), VecheckError> {
    match key {
        "kernel" => waiver.kernel = unquote(value)?,
        "variant" => waiver.variant = unquote(value)?,
        "reason" => waiver.reason = unquote(value)?,
        "upstream" => waiver.upstream = unquote(value)?,
        "expires" => waiver.expires = Date::parse(&unquote(value)?)?,
        "cost_pct" => waiver.cost_pct = parse_number(value, line)?,
        _ => return Err(line_error(line, format!("unknown waiver key '{key}'"))),
    }
    Ok(())
}

fn parse_part<T: std::str::FromStr>(
    part: Option<&str>,
    name: &str,
    full: &str,
) -> Result<T, VecheckError> {
    part.ok_or_else(|| VecheckError::new(format!("invalid {name} in date '{full}'")))?
        .parse::<T>()
        .map_err(|_| VecheckError::new(format!("invalid {name} in date '{full}'")))
}

fn parse_number(value: &str, line: usize) -> Result<f64, VecheckError> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| line_error(line, format!("expected decimal number, got '{value}'")))?;
    if parsed.is_finite() && parsed >= 0.0 {
        Ok(parsed)
    } else {
        Err(line_error(
            line,
            format!("expected non-negative finite number, got '{value}'"),
        ))
    }
}

fn strip_comment(raw: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == '#' && !quoted {
            return raw.get(..index).map_or(raw, |prefix| prefix);
        }
    }
    raw
}

fn unquote(value: &str) -> Result<String, VecheckError> {
    let Some(inner) = value
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(VecheckError::new(format!(
            "expected quoted string, got '{value}'"
        )));
    };
    let mut output = String::new();
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            match ch {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                '\\' => output.push('\\'),
                '"' => output.push('"'),
                other => {
                    output.push('\\');
                    output.push(other);
                }
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            output.push(ch);
        }
    }
    if escaped {
        return Err(VecheckError::new("unterminated string escape"));
    }
    Ok(output)
}

fn section_name(value: &str) -> Result<String, VecheckError> {
    let value = value.trim();
    if value.starts_with('"') {
        return unquote(value);
    }
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(VecheckError::new(format!(
            "expected an ISA tier name, got '{value}'"
        )));
    }
    Ok(value.to_owned())
}

fn parse_string_array(value: &str) -> Result<Vec<String>, VecheckError> {
    let Some(inner) = value
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(VecheckError::new(format!(
            "expected string array, got '{value}'"
        )));
    };
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner.split(',').map(unquote).collect()
}

fn line_error(line: usize, message: impl fmt::Display) -> VecheckError {
    VecheckError::new(format!("vecheck.toml:{}: {message}", line + 1))
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.rem_euclid(4) == 0
            && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0) =>
        {
            29
        }
        2 => 28,
        _ => 0,
    }
}

#[allow(
    clippy::integer_division,
    reason = "this is the integer Gregorian civil-date conversion; floating point would lose date precision"
)]
fn civil_from_days(days: i64) -> Date {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    Date {
        year: i32::try_from(year + i64::from(month <= 2)).unwrap_or(i32::MAX),
        month: u8::try_from(month).unwrap_or(12),
        day: u8::try_from(day).unwrap_or(31),
    }
}

/// Parsed result for one LLVM optimization remark document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remark {
    /// `Passed` or `Missed`.
    pub kind: RemarkKind,
    /// LLVM pass name.
    pub pass: String,
    /// Demangled function text emitted by LLVM.
    pub function: String,
    /// LLVM's reason text, preserved verbatim for failures.
    pub detail: String,
}

/// LLVM loop-vectorizer remark outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemarkKind {
    /// LLVM emitted a successful vectorization remark.
    Passed,
    /// LLVM emitted a failed vectorization remark.
    Missed,
}

/// Parse all loop-vectorization records in a YAML optimization-remark stream.
#[must_use]
pub fn parse_remarks(source: &str) -> Vec<Remark> {
    source
        .split("---")
        .filter_map(parse_remark)
        .filter(|remark| remark.pass == "loop-vectorize")
        .collect()
}

fn parse_remark(document: &str) -> Option<Remark> {
    let kind = if document.contains("!Passed") {
        RemarkKind::Passed
    } else if document.contains("!Missed") {
        RemarkKind::Missed
    } else {
        return None;
    };
    let pass = yaml_scalar(document, "Pass")?;
    let function = yaml_scalar(document, "Function")?;
    let detail = document
        .lines()
        .filter_map(|line| line.split_once("String:").map(|(_, value)| value.trim()))
        .map(|value| value.trim_matches('"').trim_matches('\''))
        .collect::<Vec<_>>()
        .join(" ");
    Some(Remark {
        kind,
        pass,
        function,
        detail,
    })
}

fn yaml_scalar(document: &str, name: &str) -> Option<String> {
    document.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim().trim_matches('"').trim_matches('\'').to_owned())
    })
}

/// Verify that every configured symbol has a Passed remark and no Missed remark.
///
/// # Errors
///
/// Returns the matching LLVM Missed reason verbatim, or reports an absent
/// Passed record.
pub fn verify_remarks(config: &Config, source: &str) -> Result<(), VecheckError> {
    let remarks = parse_remarks(source);
    for kernel in config.kernels() {
        let matches = remarks
            .iter()
            .filter(|remark| remark.function.contains(&kernel.symbol))
            .collect::<Vec<_>>();
        if let Some(missed) = matches
            .iter()
            .find(|remark| remark.kind == RemarkKind::Missed)
        {
            return Err(VecheckError::new(format!(
                "{}: loop-vectorize missed for {}: {}",
                kernel.id, kernel.symbol, missed.detail
            )));
        }
        if !matches
            .iter()
            .any(|remark| remark.kind == RemarkKind::Passed)
        {
            return Err(VecheckError::new(format!(
                "{}: no loop-vectorize Passed remark for {}",
                kernel.id, kernel.symbol
            )));
        }
    }
    Ok(())
}

/// Verify one target-specific assembly expectation against text from `cargo asm`.
///
/// Patterns are literal alternatives separated by `|`; a leading or trailing
/// `\\b` requires a word boundary. This intentionally covers instruction names
/// and the no-call/no-panic checks without embedding a second regex engine.
///
/// # Errors
///
/// Returns [`VecheckError`] for an unknown kernel/tier, a missing requirement,
/// a forbidden match, or an instruction-count regression.
pub fn verify_assembly(
    config: &Config,
    target: &str,
    kernel_id: &str,
    assembly: &str,
) -> Result<(), VecheckError> {
    let kernel = config
        .kernel(kernel_id)
        .ok_or_else(|| VecheckError::new(format!("unknown kernel '{kernel_id}'")))?;
    let expectation = kernel.expectations.get(target).ok_or_else(|| {
        VecheckError::new(format!(
            "{kernel_id}: no assembly expectation for target '{target}'"
        ))
    })?;
    if let Some(selector) = &kernel.asm_symbol {
        let emitted = assembly
            .lines()
            .map(str::trim)
            .find(|line| line.ends_with(':') && !line.starts_with('.'))
            .and_then(|line| line.strip_suffix(':'))
            .ok_or_else(|| {
                VecheckError::new(format!(
                    "{kernel_id}: assembly has no emitted function symbol"
                ))
            })?;
        if !emitted.contains(selector) {
            return Err(VecheckError::new(format!(
                "{kernel_id}: expected emitted symbol containing '{selector}', got '{emitted}'"
            )));
        }
    }
    for pattern in &expectation.require {
        if !assembly_matches(assembly, pattern) {
            return Err(VecheckError::new(format!(
                "{kernel_id}: required assembly pattern '{pattern}' was absent"
            )));
        }
    }
    for pattern in &expectation.forbid {
        if assembly_matches(assembly, pattern) {
            return Err(VecheckError::new(format!(
                "{kernel_id}: forbidden assembly pattern '{pattern}' matched"
            )));
        }
    }
    if let Some(maximum) = expectation.max_insns {
        let loop_body = hot_loop_body(assembly, &expectation.require)?;
        let count = instruction_count(&loop_body);
        if count > maximum {
            return Err(VecheckError::new(format!(
                "{kernel_id}: {count} instructions exceed maximum {maximum}"
            )));
        }
    }
    Ok(())
}

/// Execute `cargo asm` and return its stdout for one configured kernel.
///
/// # Errors
///
/// Returns [`VecheckError`] when `cargo asm` cannot run or rejects the symbol.
pub fn cargo_asm(kernel: &Kernel) -> Result<String, VecheckError> {
    let mut command = if let Some(path) = std::env::var_os("VACO_CARGO_ASM") {
        Command::new(path)
    } else {
        let mut command = Command::new("cargo");
        command.arg("asm");
        command
    };
    command
        .arg("--package")
        .arg(&kernel.package)
        .arg("--simplify");
    let explicit_target = std::env::var_os("VACO_VECHECK_ASM_TARGET_DIR");
    let parent_target = std::env::var_os("CARGO_TARGET_DIR");
    let target_dir = choose_asm_target_dir(explicit_target.as_deref(), parent_target.as_deref());
    command.arg("--target-dir").arg(target_dir);
    if let Some(target) = &kernel.cargo_target {
        command.arg("--target").arg(target);
    }
    if let Some(cpu) = &kernel.cargo_target_cpu {
        command.arg("--target-cpu").arg(cpu);
    }
    if let Some(artifact) = &kernel.cargo_artifact {
        command.arg(artifact);
    }
    let selector = kernel.asm_symbol.as_deref().unwrap_or(&kernel.symbol);
    let output = command.arg(selector).output().map_err(|error| {
        VecheckError::new(format!("{}: could not start cargo asm: {error}", kernel.id))
    })?;
    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|error| {
            VecheckError::new(format!("{}: cargo asm was not UTF-8: {error}", kernel.id))
        })
    } else {
        Err(VecheckError::new(format!(
            "{}: cargo asm failed: {}",
            kernel.id,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn choose_asm_target_dir(explicit: Option<&OsStr>, parent: Option<&OsStr>) -> PathBuf {
    explicit.map_or_else(
        || {
            parent
                .map_or_else(|| PathBuf::from("target"), PathBuf::from)
                .join("vaco-vecheck-asm")
        },
        PathBuf::from,
    )
}

fn assembly_matches(assembly: &str, pattern: &str) -> bool {
    pattern.split('|').any(|alternative| {
        let starts_boundary = alternative.starts_with("\\b");
        let ends_boundary = alternative.ends_with("\\b");
        let literal = alternative
            .trim_start_matches("\\b")
            .trim_end_matches("\\b");
        find_literal(assembly, literal).any(|index| {
            let before = assembly
                .get(..index)
                .and_then(|prefix| prefix.chars().next_back());
            let after = assembly
                .get(index + literal.len()..)
                .and_then(|suffix| suffix.chars().next());
            (!starts_boundary || before.is_none_or(|ch| !is_word(ch)))
                && (!ends_boundary || after.is_none_or(|ch| !is_word(ch)))
        })
    })
}

fn find_literal<'a>(haystack: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    haystack.match_indices(needle).map(|(index, _)| index)
}

fn is_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn instruction_count(assembly: &str) -> usize {
    assembly
        .lines()
        .filter_map(|line| line.split('#').next())
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with(':') && !line.starts_with('.'))
        .filter(|line| {
            line.chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic())
        })
        .count()
}

fn hot_loop_body(assembly: &str, requirements: &[String]) -> Result<String, VecheckError> {
    let lines = assembly.lines().collect::<Vec<_>>();
    let mut labels = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(label) = line.trim().strip_suffix(':') {
            labels.insert(label, index);
        }
    }

    let mut candidates = Vec::new();
    for (end, line) in lines.iter().enumerate() {
        let Some(target) = branch_target(line) else {
            continue;
        };
        let Some(&start) = labels.get(target) else {
            continue;
        };
        if start >= end {
            continue;
        }
        let body = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| (index > start && index <= end).then_some(*line))
            .collect::<Vec<_>>()
            .join("\n");
        if requirements
            .iter()
            .all(|pattern| assembly_matches(&body, pattern))
        {
            candidates.push(body);
        }
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(VecheckError::new(
            "no backward-edge loop contains every required assembly pattern",
        )),
        count => Err(VecheckError::new(format!(
            "{count} backward-edge loops contain every required assembly pattern"
        ))),
    }
}

fn branch_target(line: &str) -> Option<&str> {
    let mut parts = line.split_whitespace();
    let opcode = parts.next()?;
    if !opcode.starts_with('j') {
        return None;
    }
    parts.next().map(|target| target.trim_end_matches(','))
}

/// Error returned by malformed contracts and failed vectorization assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VecheckError(String);

impl VecheckError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Construct an argument or environment error for the command-line tool.
    #[must_use]
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(message)
    }
}

impl fmt::Display for VecheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for VecheckError {}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::choose_asm_target_dir;

    #[test]
    fn automatic_asm_uses_a_child_target_dir_outside_the_parent_cargo_lock() {
        let parent = OsStr::new("/private/tmp/vaco-vecheck-target");
        assert_eq!(
            choose_asm_target_dir(None, Some(parent)),
            PathBuf::from("/private/tmp/vaco-vecheck-target/vaco-vecheck-asm")
        );
    }

    #[test]
    fn explicit_asm_target_dir_is_not_rewritten() {
        let explicit = OsStr::new("/private/tmp/dedicated-asm");
        let parent = OsStr::new("/private/tmp/vaco-vecheck-target");
        assert_eq!(
            choose_asm_target_dir(Some(explicit), Some(parent)),
            PathBuf::from("/private/tmp/dedicated-asm")
        );
    }
}
