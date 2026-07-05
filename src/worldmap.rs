//! Draws the world as filled land on dark water.
//!
//! Flat shapes, hard coastlines, no gradients and no texture — the same
//! rectangular, high-contrast language as the rest of the client.
//!
//! Vector, not raster. A bitmap of land would go blocky the moment the view
//! zoomed past its own resolution; these are the coastline rings themselves, so
//! the outline stays exact at every scale and only the visible window is ever
//! rasterised. `tiny-skia` does the filling on the CPU — it is already in the
//! tree under Slint's SVG support — so nothing here asks anything of the GPU.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tiny_skia::{FillRule, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Coastline rings, built by `make_coast.py` from the CC0 Natural Earth blank
/// map. Little endian: a ring count, then per ring a point count and that many
/// `f32` (longitude, latitude) pairs.
///
/// Float, not fixed point. Hundredths of a degree read cleanly at a whole-world
/// view and turned into visible stair-steps past about 20x, where that lattice
/// is wider than a pixel.
static COAST: &[u8] = include_bytes!("../ui/assets/world-coast.bin");

/// Latitude band the map covers. Antarctica is left out — no exit will ever sit
/// there, and it is a third of the height.
pub const LAT_TOP: f32 = 83.0;
pub const LAT_BOTTOM: f32 = -56.0;
const LAT_SPAN: f32 = LAT_TOP - LAT_BOTTOM;

/// Border line width and the smallest feature worth drawing, in screen pixels.
/// The caller draws larger than the pane and lets Slint scale down, so both are
/// multiplied by that factor to stay true on screen.
const BORDER_PX: f32 = 0.9;
const SMALLEST_FEATURE_PX: f32 = 1.6;
/// Two points closer together than this on screen cannot describe anything the
/// rasteriser could draw differently, so only one of them is sent. At a
/// whole-world view the source carries some eighty points per pixel column;
/// handing all of them to tiny-skia was most of what a zoom step paid for.
const POINT_STEP_PX: f32 = 0.35;
const SUPERSAMPLE_HINT: f32 = 2.0;
/// Borders start showing at this scale and are fully drawn by this one.
const BORDER_FROM: f32 = 1.4;
const BORDER_FULL: f32 = 3.0;

/// The map is this many times wider than it is tall. `world-map.slint` repeats
/// the ratio to keep its markers on the coastline.
pub const ASPECT: f32 = 360.0 / LAT_SPAN;

/// The visible window, in the same 0..1 space the markers use.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl View {
    /// Whether every point of `inner` lies within this window. What decides
    /// that a texture already drawn can serve the window on show — and, if it
    /// were ever wrong, exactly what would leave a blank strip along an edge.
    pub fn contains(&self, inner: &View) -> bool {
        const SLACK: f32 = 1e-6;
        inner.x >= self.x - SLACK
            && inner.y >= self.y - SLACK
            && inner.x + inner.w <= self.x + self.w + SLACK
            && inner.y + inner.h <= self.y + self.h + SLACK
    }
}

/// `view` grown by `factor` about its centre and pushed back inside the world.
///
/// The extra is the margin a drag moves into before anything has to be drawn
/// again. Growth is equal on both axes, so the land keeps its proportions, and
/// it is capped at the width of the world: a whole-world view grows not at all,
/// which is why the most expensive view to draw is also the one that never
/// needs a margin — you cannot pan past the edges of the world.
pub fn overscan(view: View, factor: f32) -> View {
    if view.w <= 0.0 || view.h <= 0.0 {
        return view;
    }
    let w = (view.w * factor.max(1.0)).min(1.0).max(view.w);
    let grown = w / view.w;
    let h = view.h * grown;
    let x = (view.x - (w - view.w) / 2.0).clamp(0.0, (1.0 - w).max(0.0));
    // A window taller than the world has slack above and below whatever it is
    // shown at, so it centres instead of clamping.
    let y = if h >= 1.0 {
        (1.0 - h) / 2.0
    } else {
        (view.y - (h - view.h) / 2.0).clamp(0.0, 1.0 - h)
    };
    View { x, y, w, h }
}

struct Rings {
    /// Every ring's points, laid end to end.
    points: Vec<(f32, f32)>,
    /// Where each ring starts and ends inside `points`.
    spans: Vec<(u32, u32)>,
}

fn rings() -> &'static Rings {
    static RINGS: std::sync::OnceLock<Rings> = std::sync::OnceLock::new();
    RINGS.get_or_init(|| {
        let mut points = Vec::new();
        let mut spans = Vec::new();
        let mut at = 0usize;

        let read_u32 = |at: &mut usize| {
            let value = u32::from_le_bytes(COAST[*at..*at + 4].try_into().unwrap());
            *at += 4;
            value
        };
        let read_u16 = |at: &mut usize| {
            let value = u16::from_le_bytes(COAST[*at..*at + 2].try_into().unwrap());
            *at += 2;
            value
        };
        let read_f32 = |at: &mut usize| {
            let value = f32::from_le_bytes(COAST[*at..*at + 4].try_into().unwrap());
            *at += 4;
            value
        };

        let count = read_u32(&mut at);
        for _ in 0..count {
            let n = read_u16(&mut at) as usize;
            let start = points.len() as u32;
            for _ in 0..n {
                let lon = read_f32(&mut at);
                let lat = read_f32(&mut at);
                // Straight to the 0..1 space the view is expressed in.
                points.push((
                    (lon + 180.0) / 360.0,
                    (LAT_TOP - lat) / LAT_SPAN,
                ));
            }
            spans.push((start, points.len() as u32));
        }
        Rings { points, spans }
    })
}

/// Renders `view` into a `width` by `height` image.
///
/// Land is filled and then outlined. The outlines are the country polygons the
/// source carries, so they are national borders wherever two countries meet and
/// coastline everywhere else.
///
/// Detail follows the scale, the way a game map does: borders fade in as the
/// view closes, and a ring too small to read at the current scale is skipped
/// rather than drawn as a speck. Zoomed out you get the shape of the world;
/// zoomed in you get its divisions.
///
/// `land` and `border` are RGBA. Land is deliberately not opaque — the window
/// behind it is part of the design.
pub fn render(view: View, width: u32, height: u32, land: [u8; 4], border: [u8; 4]) -> Image {
    let width = width.max(1);
    let height = height.max(1);

    let Some(mut pixmap) = Pixmap::new(width, height) else {
        return Image::default();
    };
    if view.w <= 0.0 || view.h <= 0.0 {
        return from_pixmap(pixmap);
    }

    // 0..1 world space to pixels, for the window on show.
    let sx = width as f32 / view.w;
    let sy = height as f32 / view.h;
    let to_screen = Transform::from_row(sx, 0.0, 0.0, sy, -view.x * sx, -view.y * sy);

    // Everything outside the window, with a margin so a ring that only clips
    // the edge still gets drawn.
    let left = view.x - view.w * 0.05;
    let right = view.x + view.w * 1.05;
    let top = view.y - view.h * 0.05;
    let bottom = view.y + view.h * 1.05;

    // Anything narrower than this on screen is noise at this scale.
    let smallest = SMALLEST_FEATURE_PX * SUPERSAMPLE_HINT;

    // Both thresholds are quoted in final screen pixels, and the pixmap is
    // drawn supersampled, so convert once here.
    let step = POINT_STEP_PX * SUPERSAMPLE_HINT;
    let step_squared = step * step;

    let data = rings();
    let mut builder = PathBuilder::new();
    let mut any = false;
    let mut kept: Vec<(f32, f32)> = Vec::with_capacity(256);

    for &(start, end) in &data.spans {
        let ring = &data.points[start as usize..end as usize];
        if ring.len() < 3 {
            continue;
        }

        // A ring nowhere near the window costs one bounds check, not a fill.
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for &(x, y) in ring {
            lo_x = lo_x.min(x);
            hi_x = hi_x.max(x);
            lo_y = lo_y.min(y);
            hi_y = hi_y.max(y);
        }
        if hi_x < left || lo_x > right || hi_y < top || lo_y > bottom {
            continue;
        }
        // Too small to read here. Zooming in raises sx, so it comes back.
        if (hi_x - lo_x) * sx < smallest && (hi_y - lo_y) * sy < smallest {
            continue;
        }

        // Thin the ring down to what this scale can actually show.
        kept.clear();
        kept.push(ring[0]);
        for &(x, y) in &ring[1..] {
            let last = kept[kept.len() - 1];
            let dx = (x - last.0) * sx;
            let dy = (y - last.1) * sy;
            if dx * dx + dy * dy >= step_squared {
                kept.push((x, y));
            }
        }
        // A ring reduced below a triangle has no area left to fill, so it goes
        // out whole rather than as a sliver.
        let outline: &[(f32, f32)] = if kept.len() >= 3 { &kept } else { ring };

        builder.move_to(outline[0].0, outline[0].1);
        for &(x, y) in &outline[1..] {
            builder.line_to(x, y);
        }
        builder.close();
        any = true;
    }

    if !any {
        return from_pixmap(pixmap);
    }
    let Some(path) = builder.finish() else {
        return from_pixmap(pixmap);
    };

    let mut paint = Paint::default();
    paint.set_color_rgba8(land[0], land[1], land[2], land[3]);
    paint.anti_alias = true;

    // Even-odd, so a lake punched out of a landmass stays water — and so two
    // rings that overlap do not darken each other, which matters once the fill
    // stops being opaque.
    pixmap.fill_path(&path, &paint, FillRule::EvenOdd, to_screen, None);

    let fade = border_fade(view.w);
    if fade > 0.0 {
        let mut ink = Paint::default();
        ink.set_color_rgba8(
            border[0],
            border[1],
            border[2],
            (border[3] as f32 * fade) as u8,
        );
        ink.anti_alias = true;

        let mut stroke = Stroke::default();
        // Constant on screen: the transform scales the width along with the
        // path, so divide it back out.
        stroke.width = BORDER_PX * SUPERSAMPLE_HINT / sx;
        stroke.line_join = LineJoin::Round;

        pixmap.stroke_path(&path, &ink, &stroke, to_screen, None);
    }

    from_pixmap(pixmap)
}

/// How much of the border colour shows at this scale: nothing at a whole-world
/// view, everything once the view is close enough for divisions to mean
/// something.
fn border_fade(view_w: f32) -> f32 {
    let zoom = 1.0 / view_w.max(f32::MIN_POSITIVE);
    ((zoom - BORDER_FROM) / (BORDER_FULL - BORDER_FROM)).clamp(0.0, 1.0)
}

fn from_pixmap(pixmap: Pixmap) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(pixmap.width(), pixmap.height());
    // Straight across: tiny-skia already keeps premultiplied RGBA and that is
    // exactly what `from_rgba8_premultiplied` expects. Demultiplying here and
    // still declaring it premultiplied is what put a bright fringe on every
    // coastline — the partly covered pixels along an anti-aliased edge came out
    // too light, which read as a hard, jagged border.
    for (out, src) in buffer.make_mut_slice().iter_mut().zip(pixmap.pixels()) {
        *out = Rgba8Pixel {
            r: src.red(),
            g: src.green(),
            b: src.blue(),
            a: src.alpha(),
        };
    }
    Image::from_rgba8_premultiplied(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: [u8; 4] = [43, 49, 58, 196];
    const BORDER: [u8; 4] = [125, 135, 152, 168];

    fn opaque(view: View, width: u32, height: u32) -> usize {
        let image = render(view, width, height, LAND, BORDER);
        let buffer = image.to_rgba8().unwrap();
        buffer.as_slice().iter().filter(|p| p.a > 0).count()
    }


    #[test]
    fn a_margin_always_holds_the_window_it_grew_from() {
        // Corners, edges and the middle, at scales from the whole world in.
        for &w in &[1.0_f32, 0.7, 0.4, 0.15, 0.05, 0.02] {
            for &x in &[0.0_f32, 0.5 - w / 2.0, 1.0 - w] {
                for &y in &[0.0_f32, 0.5 - w / 2.0, 1.0 - w] {
                    let view = View { x: x.max(0.0), y: y.max(0.0), w, h: w * 0.74 };
                    let region = overscan(view, 1.7);
                    assert!(
                        region.contains(&view),
                        "margin lost the window at {view:?}",
                    );
                    assert!(region.x >= -1e-6 && region.x + region.w <= 1.0 + 1e-6);
                }
            }
        }
    }

    #[test]
    fn the_whole_world_is_not_grown() {
        let whole = View { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let region = overscan(whole, 1.7);
        assert!((region.w - 1.0).abs() < 1e-6);
        assert!((region.h - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_margin_leaves_room_to_move() {
        let view = View { x: 0.4, y: 0.4, w: 0.2, h: 0.15 };
        let region = overscan(view, 1.7);
        // Room on every side, which is what a drag spends before a redraw.
        assert!(region.x < view.x && region.y < view.y);
        assert!(region.x + region.w > view.x + view.w);
        assert!(region.y + region.h > view.y + view.h);
    }

    #[test]
    fn a_window_off_the_edge_still_gets_held() {
        let view = View { x: 0.0, y: 0.0, w: 0.3, h: 0.2 };
        let region = overscan(view, 1.7);
        assert!(region.contains(&view));
        assert!(region.x >= 0.0);
    }

    #[test]
    fn the_rings_parse() {
        let data = rings();
        assert!(data.spans.len() > 500, "expected the world, got {} rings", data.spans.len());
        assert!(data.points.len() > 10_000);
        // Everything lands inside the 0..1 window the view is expressed in.
        // Latitude may run past 1 at the very top of Greenland, which the band
        // clips rather than the extractor.
        for &(x, y) in &data.points {
            assert!((-0.001..=1.001).contains(&x), "longitude out of range: {x}");
            assert!((-0.2..=1.4).contains(&y), "latitude wildly out of range: {y}");
        }
    }

    #[test]
    fn draws_land_and_leaves_water_alone() {
        // Central Europe: lon 5..15, lat 45..52.
        let land = opaque(View { x: 0.514, y: 0.223, w: 0.028, h: 0.05 }, 600, 240);
        // Open south Pacific: lon -140..-130, lat -30..-37.
        let sea = opaque(View { x: 0.111, y: 0.813, w: 0.028, h: 0.05 }, 600, 240);
        assert!(land > 0, "central Europe should be filled");
        assert_eq!(sea, 0, "the open south Pacific should be empty");
    }

    #[test]
    fn renders_without_panicking_at_both_ends() {
        let full = View { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        render(full, 900, 350, LAND, BORDER);
        render(View { x: 0.48, y: 0.2, w: 0.01, h: 0.004 }, 900, 350, LAND, BORDER);
        render(full, 1, 1, LAND, BORDER);
        render(View { x: -0.2, y: -0.3, w: 1.4, h: 1.6 }, 400, 200, LAND, BORDER);
    }

    #[test]
    fn borders_arrive_with_the_zoom() {
        // Nothing at a whole-world view, everything once close in. That ramp is
        // the level of detail: the outline of the world first, its divisions
        // only when they can be read.
        assert_eq!(border_fade(1.0), 0.0, "no borders on the whole world");
        assert_eq!(border_fade(1.0 / BORDER_FROM), 0.0, "still none at the threshold");
        assert_eq!(border_fade(1.0 / BORDER_FULL), 1.0, "fully drawn by then");
        assert_eq!(border_fade(0.01), 1.0, "and stays so further in");

        let middle = border_fade(2.0 / (BORDER_FROM + BORDER_FULL));
        assert!(middle > 0.0 && middle < 1.0, "the ramp is gradual, got {middle}");
    }

    #[test]
    fn small_islands_wait_for_the_zoom() {
        // Same patch of the Aegean, drawn twice. Closer in resolves islands
        // that were below a pixel before, so more of the pane is covered.
        let wide = opaque(View { x: 0.55, y: 0.28, w: 0.06, h: 0.06 }, 500, 500);
        let close = opaque(View { x: 0.565, y: 0.29, w: 0.015, h: 0.015 }, 500, 500);
        assert!(wide > 0 && close > 0, "expected land in both, got {wide} and {close}");
    }

    #[test]
    fn zooming_in_keeps_the_coastline_exact() {
        // The same patch drawn twice as large covers about four times the
        // pixels. A raster source would plateau once it ran out of resolution.
        let near = opaque(View { x: 0.52, y: 0.24, w: 0.02, h: 0.02 }, 400, 400);
        let nearer = opaque(View { x: 0.525, y: 0.245, w: 0.01, h: 0.01 }, 400, 400);
        assert!(near > 0 && nearer > 0);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    const LAND: [u8; 4] = [43, 49, 58, 196];
    const BORDER: [u8; 4] = [125, 135, 152, 168];

    fn time(label: &str, view: View, width: u32, height: u32) {
        // One warm pass so the ring table is decoded before the clock starts.
        let _ = render(view, width, height, LAND, BORDER);
        let started = Instant::now();
        const ROUNDS: u32 = 10;
        for _ in 0..ROUNDS {
            let _ = render(view, width, height, LAND, BORDER);
        }
        let each = started.elapsed() / ROUNDS;
        println!("{label:<34} {width}x{height}  {:>7.2} ms", each.as_secs_f64() * 1000.0);
    }

    /// Not a correctness test: it prints what a single view change costs, which
    /// is what a zoom step or a drag pays. `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, not correctness"]
    fn what_a_view_change_costs() {
        let whole = View { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        let close = View { x: 0.45, y: 0.28, w: 0.06, h: 0.06 };
        time("whole world, 1x", whole, 860, 640);
        time("whole world, 2x supersampled", whole, 1720, 1280);
        time("zoomed in, 1x", close, 860, 640);
        time("zoomed in, 2x supersampled", close, 1720, 1280);
    }
}
