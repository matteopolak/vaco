//! `bm3d` — Block-Matching 3D denoiser. **Not implemented.**
//!
//! Left out of this work package deliberately rather than shipped shallow,
//! for reasons specific to `bm3d` and not shared by the other eight filters
//! in this crate:
//!
//! * **It is `N->V`, not `V->V`.** `ffmpeg -h filter=bm3d` reports "Inputs:
//!   dynamic (depending on the options)" — a `ref=true` instance takes a
//!   second input stream as an external reference/basic estimate, which is a
//!   different pad shape from every other filter here and from
//!   [`vaco_filter_graph::registry::pads`]'s fixed-count helpers. Getting
//!   that pad negotiation right is its own unit of work, not a variation on
//!   this crate's single-in/single-out `Simple` shape.
//! * **The algorithm itself is substantially larger than the other eight
//!   combined.** Block matching (grouping similar patches by exhaustive or
//!   windowed search), a 3D (2D spatial + 1D grouping-axis) transform, joint
//!   hard-thresholding or Wiener collaborative filtering, and weighted
//!   aggregation back into the image — each stage is comparable in size to
//!   one of `dctdnoiz`/`fftdnoiz`/the wavelet pair on their own, and BM3D's
//!   own literature (Dabov, Foi, Katkovnik & Egiazarian, 2007) runs it as a
//!   two-pass (`estim=basic` then `estim=final`) pipeline.
//!
//! Per this work package's brief ("if a filter is genuinely out of reach,
//! implement the rest ... and say clearly which ones you left and why"),
//! `bm3d` is that filter. It is not registered in
//! [`crate::registry::DenoiseRegistry`] and has no `DESC`.

const _: () = ();
