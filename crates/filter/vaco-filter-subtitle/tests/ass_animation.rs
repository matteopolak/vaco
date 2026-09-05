//! Real-frame regressions for point-in-time ASS motion and fades.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "integration test"
)]

use vaco_core::{Duration, Rgba};
use vaco_filter_subtitle::ass_filter::render_at;
use vaco_filter_text::TextRenderer;
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;

const SCRIPT_PREFIX: &str = "[Script Info]\nPlayResX: 320\nPlayResY: 240\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, Bold, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV\nStyle: Default,Arial,32,&H00FFFFFF,&H00000000,&H00000000,0,1,0,0,7,10,10,10\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
const MOVE_SCRIPT: &str = include_str!("data/ass-animation-move.ass");
const CLIP_SCRIPT: &str = include_str!("data/ass-animation-clip.ass");

fn frame_with_script(script: &str, time: Duration) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool.acquire_video(PixFmt::Gray8, 320, 240).unwrap();
    vaco_filter_draw::fill::fill(
        &mut frame,
        vaco_filter_draw::rect::Rect::full(320, 240),
        Rgba::BLACK,
    )
    .unwrap();
    let parsed = vaco_ass::parse(script);
    let mut renderer = TextRenderer::new();
    render_at(&parsed, &mut renderer, &mut frame, time).unwrap();
    frame
}

fn visible_bounds(frame: &Frame) -> Option<(u32, u32, u32, u32)> {
    let vaco_frame::FrameData::Video { width, height, .. } = frame.data else {
        return None;
    };
    let plane = frame.plane(0)?;
    let (mut min_x, mut min_y) = (width, height);
    let (mut max_x, mut max_y) = (0, 0);
    let mut found = false;
    for y in 0..height {
        let row = plane.row(usize::try_from(y).ok()?)?;
        for x in 0..width {
            if row.get(usize::try_from(x).ok()?).copied().unwrap_or(0) > 24 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                found = true;
            }
        }
    }
    found.then(|| (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

fn luma_sum(frame: &Frame) -> u64 {
    let Some(plane) = frame.plane(0) else {
        return 0;
    };
    (0..240)
        .filter_map(|row| plane.row(row))
        .flat_map(|row| row.iter().copied())
        .map(u64::from)
        .sum()
}

#[test]
fn move_changes_real_frame_anchor_between_event_endpoints() {
    let start = frame_with_script(MOVE_SCRIPT, Duration::ZERO);
    let end = frame_with_script(MOVE_SCRIPT, Duration::from_micros(4_999_000));
    let start_bounds = visible_bounds(&start).expect("start text must be visible");
    let end_bounds = visible_bounds(&end).expect("end text must be visible");
    assert!(
        end_bounds.0 > start_bounds.0 + 100,
        "{start_bounds:?} -> {end_bounds:?}"
    );
}

#[test]
fn fad_changes_real_frame_luma_over_event_lifetime() {
    let script = format!(
        "{SCRIPT_PREFIX}Dialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,{{\\fad(1000,1000)}}FADE\n"
    );
    let start = luma_sum(&frame_with_script(&script, Duration::ZERO));
    let middle = luma_sum(&frame_with_script(
        &script,
        Duration::from_micros(2_500_000),
    ));
    let end = luma_sum(&frame_with_script(
        &script,
        Duration::from_micros(5_000_000),
    ));
    assert_eq!(start, 0);
    assert!(middle > 0);
    assert_eq!(end, 0);
}

#[test]
fn animated_clip_reduces_real_frame_coverage_at_the_target_bound() {
    let start = luma_sum(&frame_with_script(CLIP_SCRIPT, Duration::ZERO));
    let middle = luma_sum(&frame_with_script(
        CLIP_SCRIPT,
        Duration::from_micros(2_000_000),
    ));
    let after = luma_sum(&frame_with_script(
        CLIP_SCRIPT,
        Duration::from_micros(3_500_000),
    ));
    assert!(
        start > middle,
        "clip should reduce coverage: {start} -> {middle}"
    );
    assert_eq!(after, 0);
}
