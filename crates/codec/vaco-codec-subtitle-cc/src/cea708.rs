//! CEA-708 (DTVCC) decode: packet assembly from `cc_type` 2/3 triplets,
//! service block demultiplexing, and window/pen command interpretation.
//!
//! # Scope
//!
//! Fully interpreted: `SetCurrentWindow0-7`, `ClearWindows`,
//! `DisplayWindows`, `HideWindows`, `ToggleWindows`, `DeleteWindows`,
//! `DefineWindow` (geometry), `SetPenLocation`, `SetPenAttributes`
//! (italics/underline only), `SetPenColor` (foreground only), `G0`
//! (printable ASCII plus the `0x7F` musical-note deviation) and `G1`
//! (Latin-1) text, and the `C0` cursor codes `CR`/`BS`/`FF`/`HCR`.
//!
//! Parsed for correct byte-offset tracking but not semantically applied:
//! `SetWindowAttributes` (fill/border/scroll styling), the `EXT1` escape and
//! everything behind it (the `C2`/`C3`/`G2`/`G3` code space), `P16`,
//! `Delay`/`DelayCancel`/`Reset`. A caption that relies only on window
//! geometry, pen color/style and text — the common case — decodes fully; one
//! that relies on border styling or the extended code space does not.

pub mod tables;

use crate::Event;
use crate::event::{Screen, Style};
use crate::triplet::{CcType, Triplet};
pub use tables::WindowGeometry;

/// A DTVCC packet's payload is at most 127 bytes: its header byte's length
/// field is 6 bits, so this bound comes from the wire format itself, not
/// from policy.
const MAX_PACKET_PAYLOAD: usize = 127;
const MAX_WINDOWS: usize = 8;

#[derive(Debug, Clone)]
struct Window {
    geometry: WindowGeometry,
    screen: Screen,
    pen_row: u8,
    pen_col: u8,
    pen_style: Style,
}

impl Window {
    fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            screen: Screen::new(),
            pen_row: 0,
            pen_col: 0,
            pen_style: Style::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct Service {
    current_window: Option<u8>,
    windows: [Option<Window>; MAX_WINDOWS],
}

impl Default for Service {
    fn default() -> Self {
        Self {
            current_window: None,
            windows: std::array::from_fn(|_| None),
        }
    }
}

impl Service {
    fn current_window_mut(&mut self) -> Option<&mut Window> {
        let id = self.current_window?;
        self.windows
            .get_mut(usize::from(id))
            .and_then(Option::as_mut)
    }

    fn window_mut(&mut self, id: u8) -> Option<&mut Window> {
        self.windows
            .get_mut(usize::from(id))
            .and_then(Option::as_mut)
    }

    fn for_each_bit(&mut self, bitmap: u8, mut f: impl FnMut(&mut Window)) {
        for id in 0..8u8 {
            if bitmap & (1 << id) != 0
                && let Some(w) = self.window_mut(id)
            {
                f(w);
            }
        }
    }

    fn push_window_events(&mut self, bitmap: u8, service_no: u8, events: &mut Vec<Event>) {
        for id in 0..8u8 {
            if bitmap & (1 << id) == 0 {
                continue;
            }
            let Some(w) = self.window_mut(id) else {
                continue;
            };
            let screen = w.geometry.visible.then(|| w.screen.clone());
            events.push(Event::Cea708 {
                service_no,
                window_id: id,
                geometry: w.geometry,
                screen,
            });
        }
    }

    fn write_char(&mut self, ch: char, service_no: u8, events: &mut Vec<Event>) {
        let Some(id) = self.current_window else {
            return;
        };
        let Some(w) = self.window_mut(id) else { return };
        let (row, col, style) = (w.pen_row, w.pen_col, w.pen_style);
        w.screen.row_mut(row).put(col, ch, style);
        w.pen_col = w.pen_col.saturating_add(1);
        if w.geometry.visible {
            events.push(Event::Cea708 {
                service_no,
                window_id: id,
                geometry: w.geometry,
                screen: Some(w.screen.clone()),
            });
        }
    }
}

/// Decodes DTVCC packets across every service number a stream uses.
///
/// One instance handles all of a program's services; [`crate::CcDecoder`]
/// owns one (CEA-708 is not split by field the way CEA-608 is — every DTVCC
/// triplet, regardless of which `cc_type` carried it, belongs to the same
/// packet sequence).
#[derive(Debug)]
pub struct Cea708Decoder {
    packet_payload: [u8; MAX_PACKET_PAYLOAD],
    packet_len: usize,
    packet_expected: usize,
    packet_in_progress: bool,
    services: Vec<(u8, Service)>,
}

impl Default for Cea708Decoder {
    fn default() -> Self {
        Self {
            packet_payload: [0; MAX_PACKET_PAYLOAD],
            packet_len: 0,
            packet_expected: 0,
            packet_in_progress: false,
            services: Vec::new(),
        }
    }
}

impl Cea708Decoder {
    /// Feed one `Dtvcc708PacketStart` or `Dtvcc708PacketData` triplet.
    ///
    /// A continuation triplet with no packet in progress, or a start
    /// triplet that abandons an incomplete packet, is counted in `desync`
    /// rather than silently dropped.
    pub fn feed(&mut self, triplet: Triplet, events: &mut Vec<Event>, desync: &mut u64) {
        match triplet.cc_type {
            CcType::Dtvcc708PacketStart => {
                if self.packet_in_progress {
                    *desync += 1;
                }
                let size_code = triplet.data[0] & 0x3F;
                self.packet_expected = if size_code == 0 {
                    MAX_PACKET_PAYLOAD
                } else {
                    usize::from(size_code) * 2 - 1
                };
                self.packet_len = 0;
                self.packet_in_progress = true;
                self.push_payload(triplet.data[1]);
                self.finish_if_complete(events);
            }
            CcType::Dtvcc708PacketData => {
                if !self.packet_in_progress {
                    *desync += 1;
                    return;
                }
                self.push_payload(triplet.data[0]);
                self.push_payload(triplet.data[1]);
                self.finish_if_complete(events);
            }
            CcType::Ntsc608Field1 | CcType::Ntsc608Field2 => {}
        }
    }

    fn push_payload(&mut self, byte: u8) {
        if let Some(slot) = self.packet_payload.get_mut(self.packet_len) {
            *slot = byte;
            self.packet_len += 1;
        }
    }

    fn finish_if_complete(&mut self, events: &mut Vec<Event>) {
        if self.packet_len < self.packet_expected {
            return;
        }
        let len = self.packet_expected.min(self.packet_payload.len());
        // Copy out of the fixed buffer first: `packet_payload` is a small
        // stack array (127 bytes, `Copy`), and copying it releases the
        // borrow before `process_service_blocks` needs `&mut self`.
        let payload_copy = self.packet_payload;
        let Some(payload) = payload_copy.get(..len) else {
            self.packet_in_progress = false;
            return;
        };
        self.process_service_blocks(payload, events);
        self.packet_in_progress = false;
        self.packet_len = 0;
    }

    fn process_service_blocks(&mut self, data: &[u8], events: &mut Vec<Event>) {
        let mut i = 0;
        while let Some(&byte0) = data.get(i) {
            i += 1;
            let mut service_no = (byte0 & 0xE0) >> 5;
            let block_size = usize::from(byte0 & 0x1F);
            if service_no == 7 && block_size != 0 {
                let Some(&byte1) = data.get(i) else { break };
                service_no = byte1 & 0x3F;
                i += 1;
            }
            if service_no == 0 {
                if block_size == 0 {
                    break;
                }
                i += block_size;
                continue;
            }
            let end = i.saturating_add(block_size).min(data.len());
            let Some(block) = data.get(i..end) else { break };
            self.apply_service_block(service_no, block, events);
            i = end;
        }
    }

    fn apply_service_block(&mut self, service_no: u8, block: &[u8], events: &mut Vec<Event>) {
        let mut i = 0;
        while i < block.len() {
            let rest = block.get(i..).unwrap_or(&[]);
            let len = tables::code_len(rest).max(1);
            let end = i.saturating_add(len).min(block.len());
            let Some(code) = block.get(i..end) else { break };
            self.dispatch_code(service_no, code, events);
            i = end;
        }
    }

    fn service_mut(&mut self, service_no: u8) -> &mut Service {
        if let Some(pos) = self.services.iter().position(|(n, _)| *n == service_no) {
            let Some((_, service)) = self.services.get_mut(pos) else {
                unreachable!("position just found by the same predicate")
            };
            return service;
        }
        self.services.push((service_no, Service::default()));
        let Some((_, service)) = self.services.last_mut() else {
            unreachable!("just pushed an element")
        };
        service
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch table, not repeated logic"
    )]
    fn dispatch_code(&mut self, service_no: u8, code: &[u8], events: &mut Vec<Event>) {
        let Some(&op) = code.first() else { return };
        let service = self.service_mut(service_no);
        match op {
            0x0D => {
                if let Some(w) = service.current_window_mut() {
                    w.pen_row = w.pen_row.saturating_add(1);
                    w.pen_col = 0;
                }
            }
            0x08 => {
                if let Some(w) = service.current_window_mut() {
                    let (row, col) = (w.pen_row, w.pen_col);
                    w.screen.row_mut(row).remove_before(col);
                    w.pen_col = w.pen_col.saturating_sub(1);
                }
            }
            0x0C => {
                if let Some(w) = service.current_window_mut() {
                    w.screen = Screen::new();
                    w.pen_row = 0;
                    w.pen_col = 0;
                }
            }
            0x0E => {
                if let Some(w) = service.current_window_mut() {
                    let row = w.pen_row;
                    w.screen.row_mut(row).truncate_from(0);
                    w.pen_col = 0;
                }
            }
            0x80..=0x87 => service.current_window = Some(op - 0x80),
            0x88 => {
                let bitmap = code.get(1).copied().unwrap_or(0);
                service.for_each_bit(bitmap, |w| w.screen = Screen::new());
            }
            0x89 => {
                let bitmap = code.get(1).copied().unwrap_or(0);
                service.for_each_bit(bitmap, |w| w.geometry.visible = true);
                service.push_window_events(bitmap, service_no, events);
            }
            0x8A => {
                let bitmap = code.get(1).copied().unwrap_or(0);
                service.for_each_bit(bitmap, |w| w.geometry.visible = false);
                service.push_window_events(bitmap, service_no, events);
            }
            0x8B => {
                let bitmap = code.get(1).copied().unwrap_or(0);
                service.for_each_bit(bitmap, |w| w.geometry.visible = !w.geometry.visible);
                service.push_window_events(bitmap, service_no, events);
            }
            0x8C => {
                let bitmap = code.get(1).copied().unwrap_or(0);
                for id in 0..8u8 {
                    if bitmap & (1 << id) != 0 {
                        if let Some(slot) = service.windows.get_mut(usize::from(id)) {
                            *slot = None;
                        }
                        if service.current_window == Some(id) {
                            service.current_window = None;
                        }
                    }
                }
            }
            0x90 => {
                if let (Some(w), Some(a0), Some(a1)) =
                    (service.current_window_mut(), code.get(1), code.get(2))
                {
                    let (italics, underline) = tables::pen_attributes([*a0, *a1]);
                    w.pen_style.italics = italics;
                    w.pen_style.underline = underline;
                }
            }
            0x91 => {
                if let (Some(w), Some(&a0)) = (service.current_window_mut(), code.get(1)) {
                    w.pen_style.color = tables::pen_color(a0);
                }
            }
            0x92 => {
                if let (Some(w), Some(a0), Some(a1)) =
                    (service.current_window_mut(), code.get(1), code.get(2))
                {
                    let (row, col) = tables::pen_location([*a0, *a1]);
                    w.pen_row = row;
                    w.pen_col = col;
                }
            }
            0x98..=0x9F => {
                let window_id = op & 0x07;
                if let Some(args) = code.get(1..7).and_then(|s| <[u8; 6]>::try_from(s).ok()) {
                    let geometry = tables::define_window(args);
                    if let Some(slot) = service.windows.get_mut(usize::from(window_id)) {
                        *slot = Some(Window::new(geometry));
                    }
                }
            }
            0x20..=0x7F => {
                if let Some(ch) = tables::decode_g0(op) {
                    service.write_char(ch, service_no, events);
                }
            }
            0xA0..=0xFF => {
                if let Some(ch) = tables::decode_g1(op) {
                    service.write_char(ch, service_no, events);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn start_packet(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xFF, header_byte(payload.len())];
        bytes.extend_from_slice(payload);
        bytes
    }

    fn header_byte(payload_len: usize) -> u8 {
        // packet header's size code: total payload length = code*2 - 1, so
        // code = (payload_len + 1) / 2. payload_len is always made odd by
        // the caller in these tests, matching real DTVCC packets (which pad
        // to an even total-with-header length).
        u8::try_from(payload_len.div_ceil(2)).expect("small test payload")
    }

    fn triplets_for(packet_bytes: &[u8]) -> Vec<Triplet> {
        let mut out = vec![Triplet {
            cc_type: CcType::Dtvcc708PacketStart,
            data: [packet_bytes[1], packet_bytes[2]],
        }];
        let mut i = 3;
        while i < packet_bytes.len() {
            let d1 = packet_bytes[i];
            let d2 = *packet_bytes.get(i + 1).unwrap_or(&0);
            out.push(Triplet {
                cc_type: CcType::Dtvcc708PacketData,
                data: [d1, d2],
            });
            i += 2;
        }
        out
    }

    #[test]
    fn define_window_then_text_emits_geometry_and_text() {
        // Service block: service 1, block_size 9 -> header 0x29.
        // Codes: DefineWindow0 (0x98 + 6 args), SetCurrentWindow0 (0x80),
        // 'H','i'.
        let define_args = [0x20u8, 0x00, 0x00, 0x00, 0x10, 0x00]; // visible, row0/col0 anchor, row_count 0, column_count 16
        let mut service_block = vec![0x98];
        service_block.extend_from_slice(&define_args);
        service_block.push(0x80); // SetCurrentWindow0
        service_block.push(b'H');
        service_block.push(b'i');
        let block_size = service_block.len();
        let mut payload = vec![(1u8 << 5) | u8::try_from(block_size).expect("fits")];
        payload.extend_from_slice(&service_block);
        if payload.len() % 2 == 0 {
            payload.push(0x00);
        }

        let packet = start_packet(&payload);
        let mut dec = Cea708Decoder::default();
        let mut events = Vec::new();
        let mut desync = 0;
        for t in triplets_for(&packet) {
            dec.feed(t, &mut events, &mut desync);
        }
        assert_eq!(desync, 0);
        let Event::Cea708 {
            service_no,
            window_id,
            geometry,
            screen,
        } = events.last().expect("at least one event")
        else {
            panic!("expected a Cea708 event")
        };
        assert_eq!(*service_no, 1);
        assert_eq!(*window_id, 0);
        assert!(geometry.visible);
        let screen = screen.as_ref().expect("window is visible");
        assert_eq!(screen.text(), "Hi");
    }
}
