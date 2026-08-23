//! EN 300 472 teletext data-unit structure, as carried by `dvbtxt`.
//!
//! A PES-carried teletext elementary stream is a sequence of fixed 46-byte
//! "data units": `data_unit_id`(8) — `0x02` (EBU teletext, non-subtitle),
//! `0x03` (EBU teletext subtitle) or `0xFF` (stuffing) in every broadcast
//! this project measured against — `data_unit_length`(8, always `0x2C` = 44),
//! then 44 bytes of `data_unit_data_field` (2 bytes of framing/line-address
//! plus 42 bytes of Hamming-coded page data this crate does not decode: see
//! [`crate::dvbtxt`]'s module docs for why not).
//!
//! Unlike [`crate::dvbsub::segments`], this structure genuinely is used for
//! more than probing: [`RECORD_LEN`] is a real, fixed-width record boundary
//! (not a length an attacker states, since `data_unit_length` is a constant
//! per the standard), so recognising it costs nothing a raw chunk reader
//! does not already pay and is strictly more useful framing. See
//! [`crate::dvbtxt`]'s module docs for why the registered demuxer does not
//! use it anyway (measured reference behaviour).

/// `data_unit_length`'s one legal value.
pub const DATA_UNIT_LENGTH: u8 = 0x2C;

/// `data_unit_id`(1) + `data_unit_length`(1) + 44 bytes of data.
pub const RECORD_LEN: usize = 46;

/// Whether `id` is a `data_unit_id` a teletext-in-PES stream actually uses.
#[must_use]
pub const fn is_plausible_unit_id(id: u8) -> bool {
    matches!(id, 0x02 | 0x03 | 0xFF)
}

/// How many consecutive, well-formed 46-byte data units open `data`.
#[must_use]
pub fn count_valid_records(data: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut pos = 0usize;
    while let Some(record) = data.get(pos..) {
        let Some(&id) = record.first() else {
            break;
        };
        let Some(&len) = record.get(1) else {
            break;
        };
        if !is_plausible_unit_id(id) || len != DATA_UNIT_LENGTH || record.len() < RECORD_LEN {
            break;
        }
        count = count.saturating_add(1);
        pos = pos.saturating_add(RECORD_LEN);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u8) -> [u8; RECORD_LEN] {
        let mut r = [0u8; RECORD_LEN];
        if let Some(a) = r.first_mut() {
            *a = id;
        }
        if let Some(b) = r.get_mut(1) {
            *b = DATA_UNIT_LENGTH;
        }
        r
    }

    #[test]
    fn counts_a_run_of_valid_records() {
        let mut data = Vec::new();
        data.extend_from_slice(&record(0x02));
        data.extend_from_slice(&record(0x03));
        data.extend_from_slice(&record(0xFF));
        assert_eq!(count_valid_records(&data), 3);
    }

    #[test]
    fn stops_at_the_first_invalid_record() {
        let mut data = Vec::new();
        data.extend_from_slice(&record(0x02));
        data.extend_from_slice(&[0u8; RECORD_LEN]); // id=0 not plausible
        assert_eq!(count_valid_records(&data), 1);
    }

    #[test]
    fn zero_on_a_short_buffer() {
        assert_eq!(count_valid_records(&[0x02, 0x2C]), 0);
    }

    #[test]
    fn zero_on_empty() {
        assert_eq!(count_valid_records(&[]), 0);
    }
}
