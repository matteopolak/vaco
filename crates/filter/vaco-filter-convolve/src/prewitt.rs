//! `prewitt` — the Prewitt operator, over [`crate::edge`]'s shared engine.
//!
//! See [`crate::edge`] for the option table, the measured formula and the
//! measured zero border.

use vaco_filter_core::FilterDesc;
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::edge::{self, PREWITT_GX, PREWITT_GY};

pub const DESC: FilterDesc = edge::pad_desc("prewitt", "Apply prewitt operator");

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    edge::create_two_gradient(DESC, PREWITT_GX, PREWITT_GY, 1.0, req)
}
