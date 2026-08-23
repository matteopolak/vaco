#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use super::schema as el;
use super::*;
use crate::synth;

// ------------------------------------------------------------------- VINTs

#[test]
fn element_ids_keep_their_marker() {
    // RFC 8794 section 5: the ID's marker is part of the stored value.
    assert_eq!(
        read_id(&[0x1A, 0x45, 0xDF, 0xA3], 4).unwrap(),
        (0x1A45_DFA3, 4)
    );
    assert_eq!(read_id(&[0xA3], 4).unwrap(), (0xA3, 1));
    assert_eq!(read_id(&[0x42, 0x86], 4).unwrap(), (0x4286, 2));
}

#[test]
fn a_zero_leading_octet_is_rejected_rather_than_read_as_nine_octets() {
    assert!(read_id(&[0x00, 0, 0, 0, 0], 4).is_err());
    assert!(read_size(&[0x00, 0, 0, 0, 0, 0, 0, 0, 0], 8).is_err());
}

#[test]
fn an_id_longer_than_the_cap_is_rejected() {
    // Four leading zero bits is a five-octet ID; the cap is four.
    assert!(read_id(&[0x07, 0, 0, 0, 0], 4).is_err());
}

#[test]
fn sizes_strip_their_marker() {
    assert_eq!(read_size(&[0x81], 8).unwrap(), (Size::Known(1), 1));
    assert_eq!(read_size(&[0x40, 0x7F], 8).unwrap(), (Size::Known(127), 2));
    assert_eq!(
        read_size(&[0x01, 0, 0, 0, 0, 0, 0, 0], 8).unwrap(),
        (Size::Known(0), 8)
    );
}

#[test]
fn all_ones_is_the_unknown_marker_at_every_length() {
    for len in 1..=8u8 {
        let bytes = synth::vint_unknown(len);
        assert_eq!(
            read_size(&bytes, 8).unwrap(),
            (Size::Unknown, usize::from(len)),
            "length {len}"
        );
    }
}

#[test]
fn signed_lace_vints_round_trip() {
    for v in [
        0i64, 1, -1, 63, -63, 64, -64, 8191, -8191, 8192, -8192, 1_000_000, -1_000_000,
    ] {
        let bytes = synth::signed_vint(v);
        let (got, used) = read_signed_vint(&bytes).unwrap();
        assert_eq!(got, v, "encoding of {v} was {bytes:02X?}");
        assert_eq!(used, bytes.len());
    }
}

/// RFC 9559 table 38 gives the octets for a delta of -300.
#[test]
fn the_rfc_lace_delta_example_decodes() {
    assert_eq!(read_signed_vint(&[0x5E, 0xD3]).unwrap(), (-300, 2));
    assert_eq!(read_size(&[0x43, 0x20], 8).unwrap(), (Size::Known(800), 2));
}

// ------------------------------------------------------------------ schema

#[test]
fn the_schema_is_sorted_and_free_of_duplicate_ids() {
    let mut prev = 0u32;
    for def in schema::ELEMENTS {
        assert!(def.id > prev, "{} is out of order", def.name);
        prev = def.id;
    }
}

#[test]
fn every_parent_is_itself_in_the_schema() {
    for def in schema::ELEMENTS {
        if def.parent == schema::ROOT || def.parent == schema::GLOBAL {
            continue;
        }
        let parent =
            lookup(def.parent).unwrap_or_else(|| panic!("{} names an unknown parent", def.name));
        assert_eq!(
            parent.kind,
            ElementKind::Master,
            "{}'s parent {} is not a master",
            def.name,
            parent.name
        );
    }
}

#[test]
fn global_elements_are_children_of_everything() {
    assert!(is_child_of(el::VOID, el::SEGMENT));
    assert!(is_child_of(el::VOID, el::CLUSTER));
    assert!(is_child_of(el::CRC32, el::TRACKENTRY));
}

#[test]
fn recursive_elements_may_contain_themselves() {
    assert!(is_child_of(el::SIMPLETAG, el::TAG));
    assert!(is_child_of(el::SIMPLETAG, el::SIMPLETAG));
    assert!(is_child_of(el::CHAPTERATOM, el::CHAPTERATOM));
    assert!(!is_child_of(el::TRACKENTRY, el::TRACKENTRY));
}

#[test]
fn only_segment_and_cluster_may_be_unknown_sized() {
    let allowed: Vec<_> = schema::ELEMENTS
        .iter()
        .filter(|d| d.unknown_size_ok)
        .map(|d| d.name)
        .collect();
    assert_eq!(allowed, vec!["Segment", "Cluster"]);
}

// ------------------------------------------------------ unknown-size ending

/// RFC 8794 section 6.2, table 5, applied to Matroska's own shape.
#[test]
fn a_new_cluster_ends_an_unknown_size_cluster() {
    let mut stack = MatroskaStack::new();
    stack.push(el::SEGMENT, None).unwrap();
    stack.push(el::CLUSTER, None).unwrap();
    // Sibling: shares the same parent, so it ends the open one.
    assert_eq!(stack.terminations_for(el::CLUSTER), Some(1));
    // Child: does not.
    assert_eq!(stack.terminations_for(el::SIMPLEBLOCK), Some(0));
    // A sibling of the *segment's* children ends only the cluster.
    assert_eq!(stack.terminations_for(el::CUES), Some(1));
    // A root element ends everything.
    assert_eq!(stack.terminations_for(el::SEGMENT), Some(2));
    assert_eq!(stack.terminations_for(el::EBML), Some(2));
}

#[test]
fn an_unknown_id_never_ends_an_unknown_size_element() {
    // RFC 8794 section 6.2 lists only *valid* elements as terminators, so an ID
    // the schema does not know cannot end anything — it is skipped by size.
    let mut stack = MatroskaStack::new();
    stack.push(el::SEGMENT, None).unwrap();
    stack.push(el::CLUSTER, None).unwrap();
    assert_eq!(stack.terminations_for(0x8F), None);
}

#[test]
fn a_known_size_frame_is_never_ended_early() {
    let mut stack = MatroskaStack::new();
    stack.push(el::SEGMENT, None).unwrap();
    stack.push(el::CLUSTER, Some(1000)).unwrap();
    // `Cues` is not a legal child of `Cluster`, but the cluster's size says
    // where it ends, so the answer is "skip", not "close".
    assert_eq!(stack.terminations_for(el::CUES), None);
}

#[test]
fn frames_close_when_their_end_is_reached() {
    let mut stack = MatroskaStack::new();
    stack.push(el::SEGMENT, Some(100)).unwrap();
    stack.push(el::CLUSTER, Some(50)).unwrap();
    assert_eq!(stack.close_finished(49), 0);
    assert_eq!(stack.close_finished(50), 1);
    assert_eq!(stack.close_finished(100), 1);
    assert!(stack.is_empty());
}

#[test]
fn the_stack_refuses_to_grow_past_its_ceiling() {
    let mut stack = MatroskaStack::new();
    for _ in 0..MatroskaStack::MAX_FRAMES {
        stack.push(el::SIMPLETAG, None).unwrap();
    }
    assert!(stack.push(el::SIMPLETAG, None).is_err());
}

// ------------------------------------------------------------- slice reader

#[test]
fn children_are_yielded_with_their_data_offsets() {
    let mut body = synth::uint(el::PIXELWIDTH, 320);
    let width_len = body.len();
    body.extend_from_slice(&synth::uint(el::PIXELHEIGHT, 240));
    let kids: Vec<_> = Slice::new(&body, Caps::default()).children().collect();
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0].id, el::PIXELWIDTH);
    assert_eq!(as_uint(kids[0].data), Some(320));
    assert_eq!(kids[1].offset, width_len);
    assert_eq!(as_uint(kids[1].data), Some(240));
    assert!(kids[1].data_offset > kids[1].offset);
}

#[test]
fn a_truncated_tail_ends_the_iteration_instead_of_failing() {
    let mut body = synth::uint(el::PIXELWIDTH, 320);
    body.extend_from_slice(&[0x54, 0xB0, 0x88, 0x00]); // header claiming 8 octets
    let kids: Vec<_> = Slice::new(&body, Caps::default()).children().collect();
    // The complete child survives; the truncated one is dropped.
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[1].data.len(), 1);
}

#[test]
fn a_child_can_never_claim_more_than_its_parent_holds() {
    // A one-octet body declaring a 2^40-octet child.
    let body = [0xB0, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    for child in Slice::new(&body, Caps::default()).children() {
        assert!(child.data.len() <= body.len());
    }
}

// ---------------------------------------------------------------- accessors

#[test]
fn integers_are_read_at_every_stored_width() {
    assert_eq!(as_uint(&[]), Some(0));
    assert_eq!(as_uint(&[0x01]), Some(1));
    assert_eq!(as_uint(&[0xFF; 8]), Some(u64::MAX));
    assert_eq!(as_uint(&[0; 9]), None);
    // RFC 8794 section 7.2: signed integers are sign-extended from their width.
    assert_eq!(as_int(&[0xFF]), Some(-1));
    assert_eq!(as_int(&[0x80]), Some(-128));
    assert_eq!(as_int(&[0x7F]), Some(127));
    assert_eq!(as_int(&[]), Some(0));
}

#[test]
fn floats_accept_only_the_three_defined_widths() {
    assert_eq!(as_float(&[]), Some(0.0));
    assert_eq!(as_float(&1.5f32.to_be_bytes()), Some(1.5));
    assert_eq!(as_float(&2008.0f64.to_be_bytes()), Some(2008.0));
    assert_eq!(as_float(&[0, 0]), None);
}

#[test]
fn strings_stop_at_their_first_nul() {
    // RFC 8794 section 7.4 permits zero padding after the value.
    assert_eq!(as_str(b"webm\0\0\0\0"), Some("webm"));
    assert_eq!(as_str(b"matroska"), Some("matroska"));
    assert_eq!(as_str(&[0xFF, 0xFE]), None);
}

// ------------------------------------------------------------------- caps

#[test]
fn a_header_asking_for_more_than_we_read_is_refused() {
    let mut caps = Caps::default();
    assert!(caps.adopt(5, 8).is_err());
    assert!(caps.adopt(4, 9).is_err());
    assert!(caps.adopt(4, 8).is_ok());
}

#[test]
fn a_declared_max_id_length_below_four_is_still_four() {
    // RFC 8794 section 11.2.4 gives EBMLMaxIDLength a minimum of 4, and Segment
    // needs all four octets; honouring a smaller declaration would reject files
    // every other implementation reads.
    let mut caps = Caps::default();
    caps.adopt(1, 8).unwrap();
    assert_eq!(caps.max_id_len, 4);
}
