//! Contract for the format-calibration catalogue.
//!
//! The v0.1 container plan names twenty-six black-box reference experiments
//! and one source-classification review. Keeping that inventory in code makes
//! omissions visible to the agents who need to reproduce a measured rule.

use vaco_conformance::experiments::{ExperimentKind, by_id, catalogue};

#[test]
fn format_calibration_catalogue_preserves_every_planned_identifier() {
    let catalogue = catalogue();
    let ids: Vec<_> = catalogue.iter().map(|experiment| experiment.id).collect();

    assert_eq!(
        ids.len(),
        27,
        "the plan has 26 black-box experiments plus L1"
    );
    assert_eq!(
        ids,
        vec![
            "P1", "P2", "P3", "P4", "P5", "P6", "P7", "T1", "T2", "T3", "T4", "T5", "S1", "M1",
            "M2", "M3", "M4", "M5", "M6", "M7", "K1", "K2", "K3", "K4", "A1", "N1", "L1",
        ],
        "the IDs are a stable public handle for findings and future implementations"
    );
}

#[test]
fn every_black_box_experiment_has_a_reproducible_reference_recipe() {
    let catalogue = catalogue();
    let oracle: Vec<_> = catalogue
        .iter()
        .filter(|experiment| experiment.kind == ExperimentKind::ReferenceOracle)
        .collect();

    assert_eq!(oracle.len(), 26, "L1 is documentary, not an oracle run");
    for experiment in oracle {
        assert!(
            !experiment.recipe.is_empty(),
            "{} needs a concrete ffmpeg/ffprobe recipe",
            experiment.id
        );
        assert!(
            !experiment.question.is_empty(),
            "{} needs the behaviour it is intended to settle",
            experiment.id
        );
    }
}

#[test]
fn only_l1_is_a_documentary_classification_review() {
    let documentary: Vec<_> = catalogue()
        .iter()
        .filter(|experiment| experiment.kind == ExperimentKind::DocumentReview)
        .collect();

    assert_eq!(documentary.len(), 1);
    assert_eq!(
        documentary.first().map(|experiment| experiment.id),
        Some("L1")
    );
}

#[test]
fn lookup_uses_the_stable_experiment_identifier() {
    assert_eq!(
        by_id("P3").map(|experiment| experiment.kind),
        Some(ExperimentKind::ReferenceOracle)
    );
    assert!(by_id("p3").is_none(), "identifiers are intentionally exact");
    assert!(by_id("does-not-exist").is_none());
}
