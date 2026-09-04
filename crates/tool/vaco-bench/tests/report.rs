#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test diagnostics need direct fixture assertions"
)]

#[path = "../src/report.rs"]
mod report;

use report::{ReportEntry, ReportStatus, render_html};

fn entry(benchmark: &str, status: ReportStatus) -> ReportEntry {
    ReportEntry {
        benchmark: benchmark.to_owned(),
        scope: "instantiate".to_owned(),
        outcome: "created".to_owned(),
        backend: "instant".to_owned(),
        unit: "ns".to_owned(),
        machine: "runner-a".to_owned(),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        cpu: "test cpu".to_owned(),
        rustc: "rustc test".to_owned(),
        profile: "release".to_owned(),
        git_sha: "abc123".to_owned(),
        measured_unix_ms: 1_700_000_000_123,
        samples: 11,
        iterations: 16,
        median: 100.0,
        mad: 2.0,
        min: 96.0,
        p95: 108.0,
        baseline_median: Some(95.0),
        baseline_ratio: Some(0.95),
        status,
    }
}

#[test]
fn renders_sorted_rows_with_the_injected_utc_timestamp() {
    let html = render_html(
        1_700_000_000_123,
        &[
            entry("zeta", ReportStatus::Comparable),
            entry("alpha", ReportStatus::Regression),
        ],
    );

    assert!(html.contains("2023-11-14T22:13:20.123Z"));
    assert!(html.contains("<td class=\"regression\">regression</td>"));
    assert!(html.contains("<td>comparable</td>"));
    assert!(
        html.find("<td>alpha</td>").expect("alpha row")
            < html.find("<td>zeta</td>").expect("zeta row")
    );
    assert!(html.contains("95.000 ns"));
    assert!(html.contains("0.9500"));
}

#[test]
fn escapes_every_untrusted_text_field_before_embedding_it_in_html() {
    let mut row = entry("<script>alert(1)</script>", ReportStatus::Incomparable);
    row.machine = "runner & <north>".to_owned();
    row.git_sha = "\" onmouseover=alert(1)".to_owned();

    let html = render_html(0, &[row]);

    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("runner &amp; &lt;north&gt;"));
    assert!(html.contains("&quot; onmouseover=alert(1)"));
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(!html.contains("runner & <north>"));
}

#[test]
fn rendering_is_deterministic_for_one_timestamp_and_entry_set() {
    let rows = [entry("alpha", ReportStatus::Comparable)];

    assert_eq!(
        render_html(42, &rows),
        render_html(42, &rows),
        "the report must be reproducible from JSONL and an injected timestamp"
    );
}
