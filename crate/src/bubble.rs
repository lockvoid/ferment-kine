//! The caption-backdrop silhouette: vertically stacked per-line hull rects
//! → ONE `BezPath`, to be filled ONCE. A translucent backdrop can't
//! double-blend anywhere because no point is ever covered by two fills —
//! the uniformity is structural, not calibrated (line spacing stays
//! untouched).
//!
//! Silhouette grammar (the short-form caption bubble):
//!   - adjacent lines whose edges nearly match SNAP flush (an edge delta
//!     under `2 × radius` can't host a clean fillet pair — the narrow
//!     line widens, backdrop-only, glyphs unmoved);
//!   - a real width step gets a smooth S-join: a concave flare bridging
//!     the notch on the narrow side + the wide side's convex corner;
//!   - vertically disjoint lines become separate blobs (subpaths of the
//!     same single-fill path).
//!
//! Rects are assumed to be a STACK — each line's hull starts above the
//! next's start and ends above the next's end (parley line order);
//! containment between hulls does not occur.

use vello_cpu::kurbo::{BezPath, Point, Rect};

const EPS: f64 = 1e-6;
/// Cubic Bézier kappa: control distance approximating a quarter circle.
const KAPPA: f64 = 0.552_284_749_830_793_6;

/// Build the single-fill backdrop path for a set of per-line hull rects
/// (already padded, already in final space). Order-insensitive; degenerate
/// rects are dropped.
pub(crate) fn backdrop_path(rects: &[Rect], radius: f64) -> BezPath {
    let mut stack: Vec<Rect> = rects
        .iter()
        .copied()
        .filter(|r| r.width() > EPS && r.height() > EPS)
        .collect();
    stack.sort_by(|a, b| a.y0.total_cmp(&b.y0));

    let mut path = BezPath::new();
    let mut group_start = 0;
    let mut group_bottom = f64::NEG_INFINITY;
    for i in 0..stack.len() {
        if i > group_start && stack[i].y0 > group_bottom + 0.5 {
            walk_group(&mut path, &mut stack[group_start..i], radius);
            group_start = i;
            group_bottom = f64::NEG_INFINITY;
        }
        group_bottom = group_bottom.max(stack[i].y1);
    }
    if group_start < stack.len() {
        let len = stack.len();
        walk_group(&mut path, &mut stack[group_start..len], radius);
    }
    path
}

/// A vertical run of constant boundary x on one side, top→bottom.
struct Seg {
    x: f64,
    y0: f64,
    y1: f64,
}

impl Seg {
    /// Vertical room available to a corner arc at either end of this run —
    /// half, because both ends may carry one.
    fn cap(&self) -> f64 {
        ((self.y1 - self.y0) / 2.0).max(0.0)
    }
}

fn walk_group(path: &mut BezPath, rects: &mut [Rect], radius: f64) {
    snap_edges(rects, radius);
    let rs = side_segments(rects, true);
    let ls = side_segments(rects, false);
    let top_y = rects[0].y0;
    let bottom_y = rects[rects.len() - 1].y1;

    let top_w = rects[0].width() / 2.0;
    let bottom_w = rects[rects.len() - 1].width() / 2.0;
    let r_tr = radius.min(top_w).min(rs[0].cap());
    let r_tl = radius.min(top_w).min(ls[0].cap());
    let r_br = radius.min(bottom_w).min(rs[rs.len() - 1].cap());
    let r_bl = radius.min(bottom_w).min(ls[ls.len() - 1].cap());

    // Clockwise ring: top edge → right side down → bottom edge → left side up.
    path.move_to((ls[0].x + r_tl, top_y));
    path.line_to((rs[0].x - r_tr, top_y));
    corner(path, Point::new(rs[0].x, top_y), Point::new(rs[0].x, top_y + r_tr));

    for i in 0..rs.len() - 1 {
        let (a, b) = (&rs[i], &rs[i + 1]);
        let dx = (b.x - a.x).abs();
        let ra = radius.min(a.cap()).min(dx / 2.0);
        let rb = radius.min(b.cap()).min(dx / 2.0);
        let dir = (b.x - a.x).signum();
        let step = a.y1;
        path.line_to((a.x, step - ra));
        corner(path, Point::new(a.x, step), Point::new(a.x + dir * ra, step));
        path.line_to((b.x - dir * rb, step));
        corner(path, Point::new(b.x, step), Point::new(b.x, step + rb));
    }

    let last_r = &rs[rs.len() - 1];
    path.line_to((last_r.x, bottom_y - r_br));
    corner(path, Point::new(last_r.x, bottom_y), Point::new(last_r.x - r_br, bottom_y));
    let last_l = &ls[ls.len() - 1];
    path.line_to((last_l.x + r_bl, bottom_y));
    corner(path, Point::new(last_l.x, bottom_y), Point::new(last_l.x, bottom_y - r_bl));

    for i in (1..ls.len()).rev() {
        let (a, b) = (&ls[i - 1], &ls[i]);
        let dx = (b.x - a.x).abs();
        let ra = radius.min(a.cap()).min(dx / 2.0);
        let rb = radius.min(b.cap()).min(dx / 2.0);
        let dir = (a.x - b.x).signum();
        let step = b.y0;
        path.line_to((b.x, step + rb));
        corner(path, Point::new(b.x, step), Point::new(b.x + dir * rb, step));
        path.line_to((a.x - dir * ra, step));
        corner(path, Point::new(a.x, step), Point::new(a.x, step - ra));
    }

    path.line_to((ls[0].x, top_y + r_tl));
    corner(path, Point::new(ls[0].x, top_y), Point::new(ls[0].x + r_tl, top_y));
    path.close_path();
}

/// Merge each side's boundary into constant-x runs with the step between
/// adjacent lines placed where coverage actually changes: a line sticking
/// OUT past its neighbor owns the step at its own edge (its top when it's
/// the lower line, the neighbor's bottom otherwise).
fn side_segments(rects: &[Rect], right: bool) -> Vec<Seg> {
    let x_of = |r: &Rect| if right { r.x1 } else { r.x0 };
    let sticks_out = |from: f64, to: f64| {
        if right {
            to > from + EPS
        } else {
            to < from - EPS
        }
    };
    let mut segs = vec![Seg {
        x: x_of(&rects[0]),
        y0: rects[0].y0,
        y1: rects[0].y1,
    }];
    for rect in &rects[1..] {
        let x = x_of(rect);
        let last = segs.last_mut().unwrap();
        if (x - last.x).abs() <= EPS {
            last.y1 = last.y1.max(rect.y1);
        } else if sticks_out(last.x, x) {
            let step = rect.y0.max(last.y0);
            last.y1 = step;
            segs.push(Seg { x, y0: step, y1: rect.y1 });
        } else {
            let step = last.y1.min(rect.y1);
            segs.push(Seg { x, y0: step, y1: rect.y1 });
        }
    }
    segs
}

/// Pull nearly-matching adjacent edges flush (outward only — the backdrop
/// may grow, never shrink toward the glyphs). Runs to a fixpoint: edges
/// only expand and are bounded by the group extremes.
fn snap_edges(rects: &mut [Rect], radius: f64) {
    let snap = 2.0 * radius;
    if snap <= EPS || rects.len() < 2 {
        return;
    }
    for _ in 0..16 {
        let mut changed = false;
        for i in 0..rects.len() - 1 {
            let right = rects[i].x1.max(rects[i + 1].x1);
            if (rects[i].x1 - rects[i + 1].x1).abs() > EPS
                && (rects[i].x1 - rects[i + 1].x1).abs() < snap
            {
                rects[i].x1 = right;
                rects[i + 1].x1 = right;
                changed = true;
            }
            let left = rects[i].x0.min(rects[i + 1].x0);
            if (rects[i].x0 - rects[i + 1].x0).abs() > EPS
                && (rects[i].x0 - rects[i + 1].x0).abs() < snap
            {
                rects[i].x0 = left;
                rects[i + 1].x0 = left;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Quarter-corner cubic: the two tangent lines meet at `k`; controls sit
/// `KAPPA` of the way from each endpoint toward it. Serves BOTH corner
/// kinds — the walk direction decides whether the arc reads convex (a
/// rect corner) or concave (the flare bridging a width step).
fn corner(path: &mut BezPath, k: Point, to: Point) {
    let from = match path.elements().last() {
        Some(el) => el.end_point().unwrap_or(to),
        None => to,
    };
    if (from.x - to.x).abs() <= EPS && (from.y - to.y).abs() <= EPS {
        return;
    }
    let c1 = Point::new(from.x + (k.x - from.x) * KAPPA, from.y + (k.y - from.y) * KAPPA);
    let c2 = Point::new(to.x + (k.x - to.x) * KAPPA, to.y + (k.y - to.y) * KAPPA);
    path.curve_to(c1, c2, to);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_cpu::kurbo::Shape;

    fn contains(path: &BezPath, x: f64, y: f64) -> bool {
        path.contains(Point::new(x, y))
    }

    #[test]
    fn single_line_is_a_rounded_rect() {
        let path = backdrop_path(&[Rect::new(0.0, 0.0, 200.0, 50.0)], 10.0);
        assert!(contains(&path, 100.0, 25.0), "center");
        assert!(contains(&path, 100.0, 1.0), "top edge mid");
        assert!(contains(&path, 1.0, 25.0), "left edge mid");
        assert!(!contains(&path, 2.0, 2.0), "sharp corner is rounded off");
        assert!(!contains(&path, 100.0, -1.0), "above");
        assert!(!contains(&path, 201.0, 25.0), "right of");
    }

    #[test]
    fn equal_width_lines_fuse_into_one_flush_slab() {
        let path = backdrop_path(
            &[Rect::new(0.0, 0.0, 200.0, 60.0), Rect::new(0.0, 50.0, 200.0, 110.0)],
            10.0,
        );
        assert!(contains(&path, 100.0, 55.0), "junction interior");
        // The wall must run straight through the junction — no pinch, no notch.
        for y in [40.0, 50.0, 55.0, 60.0, 70.0] {
            assert!(contains(&path, 199.0, y), "flush right wall at y={y}");
            assert!(!contains(&path, 201.0, y), "outside right wall at y={y}");
        }
    }

    #[test]
    fn near_equal_edges_snap_flush() {
        // Deltas of 8 < 2×radius: the narrow line's backdrop widens to match.
        let path = backdrop_path(
            &[Rect::new(0.0, 0.0, 200.0, 60.0), Rect::new(8.0, 50.0, 192.0, 110.0)],
            10.0,
        );
        assert!(contains(&path, 196.0, 100.0), "snapped edge covers the delta");
        assert!(contains(&path, 4.0, 100.0), "left edge snapped too");
        assert!(!contains(&path, 201.0, 100.0));
    }

    #[test]
    fn a_real_width_step_gets_a_smooth_flare() {
        // Bottom wider by 50 per side (≥ 2×radius): a true step with an
        // S-join. The flare ADDS a curved wedge the strict union lacks.
        let path = backdrop_path(
            &[Rect::new(50.0, 0.0, 350.0, 60.0), Rect::new(0.0, 50.0, 400.0, 120.0)],
            10.0,
        );
        assert!(contains(&path, 100.0, 55.0), "junction interior");
        // Inside the flare: just outside the strict union near the notch.
        assert!(contains(&path, 351.5, 48.5), "right flare bridges the notch");
        assert!(contains(&path, 48.5, 48.5), "left flare bridges the notch");
        // Beyond the flare arc (on its circle-center side): outside.
        assert!(!contains(&path, 358.0, 42.0), "right notch stays open past the arc");
        assert!(!contains(&path, 42.0, 42.0), "left notch stays open past the arc");
        // The wide line's own corners are convex-rounded as usual.
        assert!(!contains(&path, 398.5, 51.5), "wide corner rounded");
    }

    #[test]
    fn disjoint_lines_are_separate_blobs_of_one_path() {
        let path = backdrop_path(
            &[Rect::new(0.0, 0.0, 200.0, 50.0), Rect::new(0.0, 100.0, 200.0, 150.0)],
            10.0,
        );
        assert!(contains(&path, 100.0, 25.0));
        assert!(contains(&path, 100.0, 125.0));
        assert!(!contains(&path, 100.0, 75.0), "the gap between blobs stays empty");
    }

    #[test]
    fn three_lines_mixed_widths_stay_one_smooth_silhouette() {
        let path = backdrop_path(
            &[
                Rect::new(140.0, 0.0, 260.0, 60.0),
                Rect::new(40.0, 50.0, 360.0, 120.0),
                Rect::new(120.0, 110.0, 280.0, 170.0),
            ],
            10.0,
        );
        assert!(contains(&path, 200.0, 30.0));
        assert!(contains(&path, 200.0, 85.0));
        assert!(contains(&path, 200.0, 145.0));
        assert!(contains(&path, 261.5, 48.5), "flare at the top junction");
        assert!(contains(&path, 281.5, 121.5), "flare at the bottom junction");
    }

    #[test]
    fn degenerate_input_is_dropped() {
        assert!(backdrop_path(&[], 10.0).is_empty());
        assert!(backdrop_path(&[Rect::new(0.0, 0.0, 0.0, 50.0)], 10.0).is_empty());
        assert!(backdrop_path(&[Rect::new(0.0, 0.0, 200.0, 0.0)], 10.0).is_empty());
    }
}
