//! `scharr` — the Scharr operator, over [`crate::edge`]'s shared engine.
//!
//! See [`crate::edge`] for the option table and the measured `rdiv=16`
//! normalisation this operator alone needs.

use vaco_filter_core::FilterDesc;
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::edge::{self, SCHARR_GX, SCHARR_GY, SCHARR_RDIV};

pub const DESC: FilterDesc = edge::pad_desc("scharr", "Apply scharr operator");

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    edge::create_two_gradient(DESC, SCHARR_GX, SCHARR_GY, SCHARR_RDIV, req)
}
