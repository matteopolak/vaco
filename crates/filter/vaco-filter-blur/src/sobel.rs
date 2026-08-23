//! `sobel` — the Sobel operator, over [`crate::edge`]'s shared engine.
//!
//! See [`crate::edge`] for the option table, the measured formula and the
//! measured zero border.

use vaco_filter_core::FilterDesc;
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::edge::{self, SOBEL_GX, SOBEL_GY};

pub const DESC: FilterDesc = edge::pad_desc("sobel", "Apply sobel operator");

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    edge::create_two_gradient(DESC, SOBEL_GX, SOBEL_GY, 1.0, req)
}
