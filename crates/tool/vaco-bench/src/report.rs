//! Deterministic, dependency-free HTML rendering for stored benchmark rows.

#![allow(
    clippy::integer_division,
    reason = "the proleptic Gregorian UTC conversion requires exact integer quotients"
)]

/// Comparison status already decided by the shared baseline policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReportStatus {
    /// The row has no like-for-like history.
    Incomparable,
    /// The row matched history and remains above the configured threshold.
    Comparable,
    /// The row matched history and fell below the configured threshold.
    Regression,
}

impl ReportStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Incomparable => "incomparable",
            Self::Comparable => "comparable",
            Self::Regression => "regression",
        }
    }
}

/// A fully classified latest measurement suitable for a static report row.
///
/// The results-store adapter supplies the baseline fields. Keeping that policy
/// outside this renderer prevents a second, divergent comparison implementation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReportEntry {
    pub(crate) benchmark: String,
    pub(crate) scope: String,
    pub(crate) outcome: String,
    pub(crate) backend: String,
    pub(crate) unit: String,
    pub(crate) machine: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) cpu: String,
    pub(crate) rustc: String,
    pub(crate) profile: String,
    pub(crate) git_sha: String,
    pub(crate) measured_unix_ms: i64,
    pub(crate) samples: usize,
    pub(crate) iterations: usize,
    pub(crate) median: f64,
    pub(crate) mad: f64,
    pub(crate) min: f64,
    pub(crate) p95: f64,
    pub(crate) baseline_median: Option<f64>,
    pub(crate) baseline_ratio: Option<f64>,
    pub(crate) status: ReportStatus,
}

/// Render an offline HTML dashboard from already-classified benchmark rows.
///
/// `generated_unix_ms` is an explicit input so branch reports are reproducible
/// from the same result history and report-generation timestamp.
#[must_use]
pub(crate) fn render_html(generated_unix_ms: i64, entries: &[ReportEntry]) -> String {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| {
        (
            &left.machine,
            &left.os,
            &left.arch,
            &left.cpu,
            &left.rustc,
            &left.profile,
            &left.benchmark,
            &left.scope,
            &left.outcome,
            &left.backend,
            &left.unit,
        )
            .cmp(&(
                &right.machine,
                &right.os,
                &right.arch,
                &right.cpu,
                &right.rustc,
                &right.profile,
                &right.benchmark,
                &right.scope,
                &right.outcome,
                &right.backend,
                &right.unit,
            ))
    });

    let regressions = ordered
        .iter()
        .filter(|entry| entry.status == ReportStatus::Regression)
        .count();
    let mut output = String::from(concat!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
        "<title>Vaco benchmark results</title><style>",
        "body{font:14px system-ui,sans-serif;margin:2rem;color:#18212b}",
        "table{border-collapse:collapse;width:100%;font-size:12px}",
        "th,td{border:1px solid #cbd5e1;padding:.45rem;text-align:left;vertical-align:top}",
        "th{background:#edf2f7}small{display:block;color:#52606d}.regression{color:#b42318;font-weight:700}",
        "</style></head><body><h1>Vaco benchmark results</h1>"
    ));
    output.push_str("<p>Generated <time datetime=\"");
    let generated = format_utc(generated_unix_ms);
    output.push_str(&generated);
    output.push_str("\">");
    output.push_str(&generated);
    output.push_str("</time> · rows: ");
    output.push_str(&ordered.len().to_string());
    output.push_str(" · regressions: ");
    output.push_str(&regressions.to_string());
    output.push_str("</p><table><thead><tr>");
    for heading in [
        "Benchmark",
        "Metric",
        "Current",
        "Rolling baseline",
        "Ratio",
        "Status",
        "Measurement",
        "Fingerprint",
    ] {
        output.push_str("<th>");
        output.push_str(heading);
        output.push_str("</th>");
    }
    output.push_str("</tr></thead><tbody>");
    for entry in &ordered {
        output.push_str("<tr><td>");
        push_escaped(&mut output, &entry.benchmark);
        output.push_str("</td><td>");
        push_escaped(&mut output, &entry.scope);
        output.push_str("<small>");
        push_escaped(&mut output, &entry.backend);
        output.push_str(" / ");
        push_escaped(&mut output, &entry.unit);
        output.push_str(" · ");
        push_escaped(&mut output, &entry.outcome);
        output.push_str("</small></td><td>");
        output.push_str(&format_measurement(entry.median, &entry.unit));
        output.push_str("<small>MAD ");
        output.push_str(&format_measurement(entry.mad, &entry.unit));
        output.push_str(" · min ");
        output.push_str(&format_measurement(entry.min, &entry.unit));
        output.push_str(" · p95 ");
        output.push_str(&format_measurement(entry.p95, &entry.unit));
        output.push_str("</small></td><td>");
        match entry.baseline_median {
            Some(value) => output.push_str(&format_measurement(value, &entry.unit)),
            None => output.push('—'),
        }
        output.push_str("</td><td>");
        match entry.baseline_ratio {
            Some(value) => output.push_str(&format_ratio(value)),
            None => output.push('—'),
        }
        output.push_str("</td><td");
        if entry.status == ReportStatus::Regression {
            output.push_str(" class=\"regression\"");
        }
        output.push('>');
        output.push_str(entry.status.label());
        output.push_str("</td><td>");
        output.push_str(&format_utc(entry.measured_unix_ms));
        output.push_str("<small>");
        push_escaped(&mut output, &entry.git_sha);
        output.push_str(" · samples ");
        output.push_str(&entry.samples.to_string());
        output.push_str(" · iterations ");
        output.push_str(&entry.iterations.to_string());
        output.push_str("</small></td><td>");
        push_escaped(&mut output, &entry.machine);
        output.push_str("<small>");
        push_escaped(&mut output, &entry.os);
        output.push_str(" / ");
        push_escaped(&mut output, &entry.arch);
        output.push_str(" · ");
        push_escaped(&mut output, &entry.profile);
        output.push_str("</small><small>");
        push_escaped(&mut output, &entry.cpu);
        output.push_str("</small><small>");
        push_escaped(&mut output, &entry.rustc);
        output.push_str("</small></td></tr>");
    }
    output.push_str("</tbody></table></body></html>");
    output
}

fn format_measurement(value: f64, unit: &str) -> String {
    format!("{value:.3} {unit}")
}

fn format_ratio(value: f64) -> String {
    format!("{value:.4}")
}

fn push_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

fn format_utc(unix_ms: i64) -> String {
    let seconds = unix_ms.div_euclid(1_000);
    let milliseconds = unix_ms.rem_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let hours = seconds_of_day / 3_600;
    let minutes = seconds_of_day.rem_euclid(3_600) / 60;
    let seconds = seconds_of_day.rem_euclid(60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}Z")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_unix_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
