//! Geometry repair in stored coordinates: a valid geometry, after the OGC Simple Features rules
//! (OGC 06-103r4, ISO 19125-1: 6.1.11.1 Polygon, 6.1.12 MultiPolygon), for one in which
//! [`validity::check`] finds problems. Repair Geometry writes the results.
//!
//! - Vertex-level problems get fixes that move no coordinate: an unclosed ring is closed,
//!   repeated vertices are dropped, and a ring with fewer than 3 distinct points (a line part with
//!   fewer than 2) is dropped, with its part's holes when it is the part's exterior ring.
//! - Polygon-level problems (a ring crossing, touching or running back over itself, a hole not
//!   inside its exterior ring, rings or parts that overlap or touch along a line, an interior split
//!   by rings touching at points) are repaired by area. Each ring stands for the area it winds
//!   around (non-zero winding: both lobes of a bowtie count, and the direction a ring is wound in
//!   doesn't matter); a part is its exterior ring's area less the union of its holes' areas, so a
//!   hole outside its exterior ring disappears and holes that overlap merge; and the feature is
//!   the union of its parts, so parts that overlap merge. geo's boolean overlay (i_overlay) does
//!   the arithmetic on an integer grid, which moves coordinates a little ([`grid_step`],
//!   [`GRID_STEPS_MOVED`]), and writes each piece of a split interior as a part of its own.
//! - A repaired geometry is checked again and written only when it is valid. A feature too busy
//!   to check or overlay within the work bound, or without the memory for it, or one the overlay
//!   fails on, is left as it was ([`Kept`]): never dropped.
//! - Lines may cross themselves (OGC doesn't require a simple line), and points have nothing to
//!   repair. Missing geometry and non-finite coordinates can't be repaired ([`Gone`]).
//!
//! In the browser a panic in the overlay can't be caught: it stops the app there, as it would in
//! the other tools that run the overlay (Clip, Dissolve, Buffer). So its input is bounded by the
//! work charge, has finite coordinates on a grid i_overlay can represent, and its memory is asked
//! of the browser before it runs. Natively a panic is caught, and the feature is left as it was.

use crate::validity::{self, At, Checked, Code, Limit, Problem, Room};
use geo::algorithm::bool_ops::{BooleanOps, FillRule, OpType};
use geo::orient::{Direction, Orient};
use rustgis_data::{Coord, Geometry};

/// Work per vertex the overlay takes in or makes, against the same bound as a check
/// ([`validity::MAX_WORK`]): at 25, up to 2 million vertices a feature, as Buffer allows.
const OVERLAY_COST: u64 = 25;
/// The memory the overlay holds per vertex, as estimated: geo's and i_overlay's copies of the
/// rings in floats and on the grid, their segments and sort, the overlay's graph and its output.
const OVERLAY_BYTES: u64 = 160;
/// How far a coordinate may move through the overlays of one repair, in steps of the grid: a
/// vertex is rounded to the grid in each of up to three overlays (a ring's own area, a part's holes
/// taken out, the union of the parts), each time by at most half a step on each axis, and a vertex
/// the overlay adds where rings cross lies on the rounded segments, so within as much again of the
/// input's boundary. Measured in the tests, the largest move is well under one step.
pub(crate) const GRID_STEPS_MOVED: f64 = 5.0;

/// Why a feature is left as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// Checking or overlaying it needs more work than the bound.
    Work,
    /// The memory for it couldn't be had.
    Memory,
    /// geo failed on it: its relate when checking, or the overlay (a panic, natively;
    /// coordinates on no grid it can represent; or a result that isn't valid).
    Failed,
}

impl Kept {
    /// The code written for it (data: never translated).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kept::Work | Kept::Memory => Code::TooComplex.as_str(),
            Kept::Failed => "repair_failed",
        }
    }
}

/// Why a feature can't be repaired: it is written without geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gone {
    Null,
    NonFinite,
    /// Every part has too few distinct points.
    TooFewPoints,
    /// Nothing with an area is left once repaired: rings that run back over themselves, or holes
    /// covering their whole part.
    NoArea,
}

impl Gone {
    pub(crate) const ALL: [Gone; 4] = [Gone::Null, Gone::NonFinite, Gone::TooFewPoints, Gone::NoArea];

    /// The code written for it (data: never translated).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Gone::Null => Code::NullGeometry.as_str(),
            Gone::NonFinite => Code::NonFiniteCoordinate.as_str(),
            Gone::TooFewPoints => Code::TooFewPoints.as_str(),
            Gone::NoArea => "no_area",
        }
    }
}

/// A repaired geometry, and what was done to it.
#[derive(Debug, PartialEq)]
pub(crate) struct Repaired {
    pub geometry: Geometry,
    /// What was wrong with it, each once, in [`Code`] order.
    pub codes: Vec<Code>,
    /// Rings and parts left out: too few distinct points, or no area.
    pub dropped: usize,
    /// When the overlay rebuilt it, how far its coordinates may have moved, in its units.
    pub moved: Option<f64>,
}

/// What [`repair`] made of one geometry.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Nothing is wrong: written as it is.
    Valid,
    Repaired(Repaired),
    Gone(Gone),
    Kept(Kept),
}

/// Repairs one geometry, spending at most [`validity::MAX_WORK`] on each check and on the
/// overlay, and asking `has_room` whether the browser can give the overlay's memory before it runs
/// (see `web_has_room` in the tools).
pub(crate) fn repair(g: Option<&Geometry>, has_room: &dyn Fn(u64) -> bool) -> Outcome {
    repair_with(g, validity::MAX_WORK, &|_| true, has_room)
}

/// [`repair`] with a work `budget`, asking `allow` before each reservation of a buffer whether to
/// try it, as [`validity::check_with`] does: tests refuse some to stand in for running out of
/// memory.
pub(crate) fn repair_with(g: Option<&Geometry>, budget: u64, allow: &dyn Fn(usize) -> bool, has_room: &dyn Fn(u64) -> bool) -> Outcome {
    let first = validity::check_with(g, budget, allow);
    if first.problems.is_empty() {
        return Outcome::Valid;
    }
    let Some(g) = g else { return Outcome::Gone(Gone::Null) };
    if let Some(gone) = gone(&first) {
        return Outcome::Gone(gone);
    }
    if let Some(kept) = kept(&first) {
        return Outcome::Kept(kept);
    }
    let room = Room(allow);
    match g {
        Geometry::Line(parts) => match lines(parts, room) {
            Err(limit) => Outcome::Kept(kept_for(limit)),
            Ok((parts, _)) if parts.is_empty() => Outcome::Gone(Gone::TooFewPoints),
            Ok((parts, dropped)) => {
                verified(Repaired { geometry: Geometry::Line(parts), codes: first.codes(), dropped, moved: None }, budget, allow)
            }
        },
        Geometry::Polygon(polys) => polygon(polys, &first, budget, allow, has_room),
        // A point's only problems, a missing or non-finite coordinate, are handled above.
        Geometry::Point(_) | Geometry::MultiPoint(_) => Outcome::Kept(Kept::Failed),
    }
}

/// Why a checked feature can't be repaired, if it can't.
fn gone(c: &Checked) -> Option<Gone> {
    c.problems.iter().find_map(|p| match p {
        Problem::Null { .. } => Some(Gone::Null),
        Problem::NonFinite { .. } => Some(Gone::NonFinite),
        _ => None,
    })
}

/// Why a feature that wasn't checked fully is left as it was, if it wasn't.
fn kept(c: &Checked) -> Option<Kept> {
    c.problems.iter().find_map(|p| match p {
        Problem::TooComplex { limit } => Some(kept_for(*limit)),
        _ => None,
    })
}

fn kept_for(limit: Limit) -> Kept {
    match limit {
        Limit::Work => Kept::Work,
        Limit::Memory => Kept::Memory,
        Limit::Relate => Kept::Failed,
    }
}

/// `r`, once a check finds nothing wrong with its geometry; left as it was otherwise.
fn verified(r: Repaired, budget: u64, allow: &dyn Fn(usize) -> bool) -> Outcome {
    let c = validity::check_with(Some(&r.geometry), budget, allow);
    if let Some(k) = kept(&c) {
        return Outcome::Kept(k);
    }
    if c.problems.is_empty() { Outcome::Repaired(r) } else { Outcome::Kept(Kept::Failed) }
}

/// `pts` without repeated consecutive vertices, closed when `ring`, in a vector reserved fallibly.
fn cleaned(pts: &[Coord], ring: bool, room: Room) -> Result<Vec<Coord>, Limit> {
    let mut v: Vec<Coord> = room.vec(pts.len().saturating_add(usize::from(ring)))?;
    for c in pts {
        if v.last() != Some(c) {
            v.push(*c);
        }
    }
    if ring
        && let (Some(first), Some(last)) = (v.first().copied(), v.last())
        && first != *last
    {
        v.push(first);
    }
    Ok(v)
}

/// A line's parts without repeated vertices, and without the parts that have fewer than 2
/// distinct points; how many were dropped.
fn lines(parts: &[Vec<Coord>], room: Room) -> Result<(Vec<Vec<Coord>>, usize), Limit> {
    let mut out = room.vec(parts.len())?;
    let mut dropped = 0usize;
    for part in parts {
        let part = cleaned(part, false, room)?;
        // Consecutive vertices differ now: two of them are two distinct points.
        if part.len() < 2 {
            dropped += 1;
        } else {
            out.push(part);
        }
    }
    Ok((out, dropped))
}

/// A polygon's rings closed and without repeated vertices, and without the rings that have fewer
/// than 3 distinct points (an exterior ring's part goes with it, holes and all), nor parts without
/// rings; how many rings and parts were dropped, and whether anything changed.
fn rings(polys: &[Vec<Vec<Coord>>], room: Room) -> Result<(Vec<Vec<Vec<Coord>>>, usize, bool), Limit> {
    let mut out = room.vec(polys.len())?;
    let (mut dropped, mut changed) = (0usize, false);
    for part in polys {
        let mut kept: Vec<Vec<Coord>> = room.vec(part.len())?;
        for (r, ring) in part.iter().enumerate() {
            let fixed = cleaned(ring, true, room)?;
            if validity::distinct_up_to(&fixed, 3) < 3 {
                dropped += 1;
                changed = true;
                if r == 0 {
                    break;
                }
                continue;
            }
            changed |= fixed != *ring;
            kept.push(fixed);
        }
        if part.is_empty() {
            dropped += 1;
            changed = true;
        }
        // An exterior ring dropped leaves nothing: its holes went with it.
        if !kept.is_empty() {
            out.push(kept);
        }
    }
    Ok((out, dropped, changed))
}

/// A polygon feature: the fixes that move no coordinate, then, when the polygon-level checks
/// still find something, the rebuild by area.
fn polygon(
    polys: &[Vec<Vec<Coord>>],
    first: &Checked,
    budget: u64,
    allow: &dyn Fn(usize) -> bool,
    has_room: &dyn Fn(u64) -> bool,
) -> Outcome {
    let room = Room(allow);
    let (fixed, dropped, changed) = match rings(polys, room) {
        Ok(fixed) => fixed,
        Err(limit) => return Outcome::Kept(kept_for(limit)),
    };
    if fixed.is_empty() {
        return Outcome::Gone(Gone::TooFewPoints);
    }
    let fixed = Geometry::Polygon(fixed);
    // Checked again when something changed: rings and parts left unrelated by a ring's problem
    // (see `Checked::unrelated`) are related now.
    let rechecked;
    let second = if changed {
        rechecked = validity::check_with(Some(&fixed), budget, allow);
        &rechecked
    } else {
        first
    };
    if let Some(kept) = kept(second) {
        return Outcome::Kept(kept);
    }
    let mut codes = first.codes();
    for code in second.codes() {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    if second.problems.is_empty() {
        return Outcome::Repaired(Repaired { geometry: fixed, codes, dropped, moved: None });
    }
    let Geometry::Polygon(parts) = &fixed else { return Outcome::Kept(Kept::Failed) };
    if gone(second).is_some() {
        return Outcome::Kept(Kept::Failed);
    }

    let coords = parts.iter().flatten().map(Vec::len).fold(0usize, usize::saturating_add);
    let size = u64::try_from(coords).unwrap_or(u64::MAX).saturating_add(second.contacts.saturating_mul(2));
    if size.saturating_mul(OVERLAY_COST) > budget {
        return Outcome::Kept(Kept::Work);
    }
    if !has_room(size.saturating_mul(OVERLAY_BYTES)) {
        return Outcome::Kept(Kept::Memory);
    }
    let Some(step) = grid_step(parts.iter().flatten().flatten().map(|c| (c.x, c.y))) else { return Outcome::Kept(Kept::Failed) };
    // The rings crossing themselves, by part and ring: in order, as the problems are.
    let crossing = second.problems.iter().filter_map(|p| match p {
        Problem::SelfIntersection { at: At::Ring(p, r), .. } => Some((*p, *r)),
        _ => None,
    });
    let crossing = match room.collect(crossing.clone().count(), crossing) {
        Ok(c) => c,
        Err(limit) => return Outcome::Kept(kept_for(limit)),
    };
    let rebuilt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| by_area(parts, &crossing, room)));
    let (polys, no_area, found) = match rebuilt {
        Ok(Ok(rebuilt)) => rebuilt,
        Ok(Err(kept)) => return Outcome::Kept(kept),
        Err(_) => return Outcome::Kept(Kept::Failed),
    };
    if polys.is_empty() {
        return Outcome::Gone(Gone::NoArea);
    }
    for code in found {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    let moved = Some(step * GRID_STEPS_MOVED);
    verified(Repaired { geometry: Geometry::Polygon(polys), codes, dropped: dropped.saturating_add(no_area), moved }, budget, allow)
}

/// The step of the integer grid i_overlay rounds the points `pts` to: it takes half the larger
/// side of their extent, `h`, to about 2^29 grid units (2^(29 - round(log2 h)) units a unit of
/// the coordinates). `None` when there is no such grid: an extent that is empty, not finite, or
/// so large or so small that the scale or the step isn't a finite number.
pub(crate) fn grid_step(pts: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in pts {
        (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(x), lo_y.min(y), hi_x.max(x), hi_y.max(y));
    }
    let half = (hi_x - lo_x).max(hi_y - lo_y) * 0.5;
    if !(half.is_finite() && half > 0.0) {
        return None;
    }
    let e = 29.0 - half.log2().round();
    let (scale, step) = (e.exp2(), (-e).exp2());
    (scale.is_normal() && step.is_normal()).then_some(step)
}

/// Whether the overlay can run on `rings`: they have a grid ([`grid_step`]).
fn on_grid<'a>(rings: impl Iterator<Item = &'a geo::LineString<f64>>) -> Result<(), Kept> {
    grid_step(rings.flat_map(|r| r.0.iter().map(|c| (c.x, c.y)))).map(|_| ()).ok_or(Kept::Failed)
}

/// What the rebuild by area found that the checks may have missed: a part with a ring crossing
/// itself isn't related to its other rings or to other parts (see `Checked::unrelated`).
type Found = Vec<Code>;

/// The rebuild by area (see the module doc) of `parts`, closed rings each with 3 distinct points
/// or more, of which those in `crossing` (part, ring; sorted) cross, touch or run back over
/// themselves. The rings of the result, how many rings and parts were left without area, and what
/// it found: [`Code::HoleOutsideShell`] when some of the holes of a part with such a ring lie
/// outside its exterior ring's area, and [`Code::PartsOverlap`] when parts merged, which the union
/// tells by making fewer polygons than it was given (pieces that only touch at points stay apart).
fn by_area(parts: &[Vec<Vec<Coord>>], crossing: &[(usize, usize)], room: Room) -> Result<(Vec<Vec<Vec<Coord>>>, usize, Found), Kept> {
    let empty = geo::MultiPolygon::<f64>::new(vec![]);
    let area = |ring: &Vec<Coord>, at: (usize, usize)| -> Result<geo::MultiPolygon<f64>, Kept> {
        let ring = geo::LineString(room.collect(ring.len(), ring.iter().map(|c| geo::Coord { x: c.x, y: c.y })).map_err(kept_for)?);
        let poly = geo::Polygon::new(ring, vec![]);
        if crossing.binary_search(&at).is_err() {
            // A simple ring: its area as it is, wound as the overlay expects.
            return Ok(geo::MultiPolygon::new(vec![poly.orient(Direction::Default)]));
        }
        on_grid(std::iter::once(poly.exterior()))?;
        Ok(poly.boolean_op_with_fill_rule(&empty, OpType::Union, FillRule::NonZero))
    };
    let mut all: Vec<geo::Polygon<f64>> = Vec::new();
    let mut no_area = 0usize;
    let mut found = Found::new();
    for (p, rings) in parts.iter().enumerate() {
        let Some((shell, holes)) = rings.split_first() else { continue };
        let shell = area(shell, (p, 0))?;
        if shell.0.is_empty() {
            // Its holes go with it.
            no_area += 1;
            continue;
        }
        let mut cut: Vec<geo::Polygon<f64>> = Vec::new();
        for (h, hole) in holes.iter().enumerate() {
            let hole = area(hole, (p, h + 1))?;
            if hole.0.is_empty() {
                no_area += 1;
            }
            for piece in hole.0 {
                room.grow(&mut cut).map_err(kept_for)?;
                cut.push(piece);
            }
        }
        let part = if cut.is_empty() {
            shell
        } else {
            // The holes' areas, each wound the same way, make their union under non-zero winding.
            let cut = geo::MultiPolygon::new(cut);
            on_grid(shell.0.iter().chain(&cut.0).flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors())))?;
            let related = (0..rings.len()).all(|r| crossing.binary_search(&(p, r)).is_err());
            if !related
                && !found.contains(&Code::HoleOutsideShell)
                && !cut.boolean_op_with_fill_rule(&shell, OpType::Difference, FillRule::NonZero).0.is_empty()
            {
                found.push(Code::HoleOutsideShell);
            }
            shell.boolean_op_with_fill_rule(&cut, OpType::Difference, FillRule::NonZero)
        };
        if part.0.is_empty() {
            no_area += 1;
        }
        for piece in part.0 {
            room.grow(&mut all).map_err(kept_for)?;
            all.push(piece);
        }
    }
    let all = geo::MultiPolygon::new(all);
    let merged = if parts.len() > 1 && all.0.len() > 1 {
        on_grid(all.0.iter().flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors())))?;
        let merged = all.boolean_op_with_fill_rule(&empty, OpType::Union, FillRule::NonZero);
        if merged.0.len() < all.0.len() {
            found.push(Code::PartsOverlap);
        }
        merged
    } else {
        all
    };
    Ok((polygons(&merged, room).map_err(kept_for)?, no_area, found))
}

/// The overlay's polygons as rings, copied fallibly.
fn polygons(m: &geo::MultiPolygon<f64>, room: Room) -> Result<Vec<Vec<Vec<Coord>>>, Limit> {
    let mut out = room.vec(m.0.len())?;
    for poly in &m.0 {
        // The overlay writes closed rings of 3 points or more; anything shorter has no area.
        if poly.exterior().0.len() < 4 {
            continue;
        }
        let mut rings = room.vec(poly.interiors().len().saturating_add(1))?;
        for ring in std::iter::once(poly.exterior()).chain(poly.interiors()).filter(|r| r.0.len() >= 4) {
            rings.push(room.collect(ring.0.len(), ring.0.iter().map(|c| Coord::new(c.x, c.y)))?);
        }
        out.push(rings);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ring<'a> = &'a [(f64, f64)];

    /// Every reservation tried.
    const ANY: Room<'static> = Room(&|_| true);

    fn ring(pts: Ring) -> Vec<Coord> {
        pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()
    }
    fn multi(parts: &[&[Ring]]) -> Geometry {
        Geometry::Polygon(parts.iter().map(|rings| rings.iter().map(|r| ring(r)).collect()).collect())
    }
    fn poly(rings: &[Ring]) -> Geometry {
        multi(&[rings])
    }
    fn fix(g: &Geometry) -> Outcome {
        repair(Some(g), &|_| true)
    }
    fn repaired(g: &Geometry) -> Repaired {
        match fix(g) {
            Outcome::Repaired(r) => r,
            other => panic!("{g:?} gave {other:?}"),
        }
    }
    /// The area of a polygon geometry, by the shoelace formula over every ring (holes wound
    /// against their shell count negative, so only the magnitude per ring is used: shell minus holes).
    fn area(g: &Geometry) -> f64 {
        let ring_area = |r: &[Coord]| r.windows(2).map(|w| w[0].x * w[1].y - w[1].x * w[0].y).sum::<f64>().abs() / 2.0;
        let Geometry::Polygon(parts) = g else { panic!("a polygon") };
        parts.iter().map(|rings| ring_area(&rings[0]) - rings[1..].iter().map(|h| ring_area(h)).sum::<f64>()).sum()
    }

    const SQUARE: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.)];
    const BOWTIE: Ring = &[(0., 0.), (2., 2.), (2., 0.), (0., 2.), (0., 0.)];

    #[test]
    fn cleaned_drops_repeats_and_closes_rings() {
        let pts = ring(&[(0., 0.), (0., 0.), (1., 0.), (1., 0.), (1., 1.)]);
        assert_eq!(cleaned(&pts, true, ANY).unwrap(), ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 0.)]));
        assert_eq!(cleaned(&pts, false, ANY).unwrap(), ring(&[(0., 0.), (1., 0.), (1., 1.)]));
        // A duplicated closing point is one repeat; a closed ring isn't closed again.
        assert_eq!(cleaned(&ring(&[(0., 0.), (1., 0.), (1., 1.), (0., 0.), (0., 0.)]), true, ANY).unwrap().len(), 4);
        assert_eq!(cleaned(&[], true, ANY).unwrap(), []);
    }

    #[test]
    fn degenerate_rings_and_parts_are_dropped_and_counted() {
        let flat: Ring = &[(0., 0.), (1., 0.), (0., 0.)];
        let Geometry::Polygon(polys) = multi(&[&[SQUARE, flat], &[flat, SQUARE], &[], &[SQUARE]]) else { panic!("a polygon") };
        let (out, dropped, changed) = rings(&polys, ANY).unwrap();
        // The hole of part 1; part 2, whose exterior ring goes with its hole; part 3, without rings.
        assert_eq!(dropped, 3);
        assert!(changed);
        assert_eq!(out, vec![vec![ring(SQUARE)], vec![ring(SQUARE)]]);
        let Geometry::Polygon(clean) = poly(&[SQUARE]) else { panic!("a polygon") };
        assert_eq!(rings(&clean, ANY).unwrap(), (clean.clone(), 0, false));
        // The same length as before but not the same ring: a repeat dropped and the ring closed.
        let Geometry::Polygon(open) = poly(&[&[(0., 0.), (10., 0.), (10., 0.), (10., 10.)]]) else { panic!("a polygon") };
        assert!(rings(&open, ANY).unwrap().2);

        let (out, dropped) = lines(&[ring(&[(0., 0.), (0., 0.)]), ring(&[(0., 0.), (1., 1.), (1., 1.)]), vec![]], ANY).unwrap();
        assert_eq!((out, dropped), (vec![ring(&[(0., 0.), (1., 1.)])], 2));
    }

    #[test]
    fn a_ring_stands_for_the_area_it_winds_around() {
        // A bowtie keeps both lobes, whichever way it is wound.
        for bowtie in [BOWTIE.to_vec(), BOWTIE.iter().rev().copied().collect()] {
            let r = repaired(&poly(&[&bowtie]));
            let Geometry::Polygon(parts) = &r.geometry else { panic!() };
            assert_eq!(parts.len(), 2);
            assert_eq!(area(&r.geometry), 2.0);
            assert_eq!(r.codes, [Code::SelfIntersection]);
        }
        // A loop inside the ring, touching it at a vertex: wound against the ring, it is outside
        // (an "inverted" hole, as some formats write one); wound with it, it is inside.
        let against: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (5., 8.), (8., 5.), (5., 2.), (0., 0.)];
        let with: Ring = &[(0., 0.), (10., 0.), (10., 10.), (0., 10.), (0., 0.), (5., 2.), (8., 5.), (5., 8.), (0., 0.)];
        let r = repaired(&poly(&[against]));
        assert_eq!(area(&r.geometry), 100.0 - 24.0);
        let Geometry::Polygon(parts) = &r.geometry else { panic!() };
        assert_eq!((parts.len(), parts[0].len()), (1, 2), "one polygon with a hole touching its shell at a point");
        assert_eq!(area(&repaired(&poly(&[with])).geometry), 100.0);
    }

    #[test]
    fn grid_steps_follow_i_overlays_scale() {
        let step = |pts: &[(f64, f64)]| grid_step(pts.iter().copied());
        // Half of 10 is 5: log2 5 rounds to 2, so 2^27 units a unit.
        assert_eq!(step(&[(0., 0.), (10., 3.)]), Some((-27f64).exp2()));
        assert_eq!(step(&[(1e5, 0.), (1e5 + 10.7, 10.)]), Some((-27f64).exp2()));
        assert_eq!(step(&[(-180., -90.), (180., 90.)]), Some((-22f64).exp2()));
        assert_eq!(step(&[(1., 1.), (1., 1.)]), None, "no extent");
        assert_eq!(step(&[(-1e308, 0.), (1e308, 0.)]), None, "an extent past the largest float");
        assert_eq!(step(&[(0., 0.), (1e-300, 1e-300)]), None, "a scale past the largest float");
        assert_eq!(step(&[]), None);
        // The overlay runs only on rings with a grid: in the browser, where its failures can't be
        // caught, it never sees one it can't scale.
        let ls = |pts: &[(f64, f64)]| geo::LineString::from(pts.to_vec());
        assert_eq!(on_grid([ls(&[(0., 0.), (1., 0.), (0., 1.), (0., 0.)])].iter()), Ok(()));
        assert_eq!(on_grid([ls(&[(0., 0.), (1e-310, 0.), (0., 1e-310), (0., 0.)])].iter()), Err(Kept::Failed));
        assert_eq!(on_grid([ls(&[(-1e308, 0.), (1e308, 0.), (0., 1.), (-1e308, 0.)])].iter()), Err(Kept::Failed));
    }

    #[test]
    fn what_can_not_be_repaired_has_its_reason() {
        assert_eq!(repair(None, &|_| true), Outcome::Gone(Gone::Null));
        assert_eq!(fix(&Geometry::Polygon(vec![])), Outcome::Gone(Gone::Null));
        assert_eq!(fix(&Geometry::Point(Coord::new(f64::NAN, 0.))), Outcome::Gone(Gone::NonFinite));
        assert_eq!(fix(&poly(&[SQUARE, &[(1., 1.), (f64::INFINITY, 1.), (2., 2.), (1., 1.)]])), Outcome::Gone(Gone::NonFinite));
        assert_eq!(fix(&poly(&[&[(0., 0.), (1., 0.), (0., 0.)]])), Outcome::Gone(Gone::TooFewPoints));
        assert_eq!(fix(&Geometry::Line(vec![ring(&[(0., 0.), (0., 0.)])])), Outcome::Gone(Gone::TooFewPoints));
        // Three distinct points in a row: a ring with no area.
        assert_eq!(fix(&poly(&[&[(0., 0.), (1., 0.), (2., 0.), (0., 0.)]])), Outcome::Gone(Gone::NoArea));
        // A hole covering its whole shell.
        assert_eq!(fix(&poly(&[SQUARE, SQUARE])), Outcome::Gone(Gone::NoArea));
        assert_eq!(fix(&Geometry::Point(Coord::new(1., 0.))), Outcome::Valid);
    }

    /// A pentagram: 5 segments, each crossing two others.
    fn pentagram() -> Geometry {
        let at = |k: f64| {
            let a = (90.0 + 144.0 * k).to_radians();
            Coord::new((10.0 * a.cos() * 1e6).round() / 1e6, (10.0 * a.sin() * 1e6).round() / 1e6)
        };
        Geometry::Polygon(vec![vec![(0..=5).map(|k| if k == 5 { at(0.0) } else { at(f64::from(k)) }).collect()]])
    }

    #[test]
    fn the_work_bound_and_the_overlays_memory_leave_a_feature_as_it_was() {
        let star = pentagram();
        let first = validity::check(Some(&star), validity::MAX_WORK);
        assert_eq!(first.contacts, 5);
        // Its 6 coordinates and up to 2 vertices a contact, at 25 each: more than its checks take.
        let need = (6 + 2 * 5) * OVERLAY_COST;
        assert_eq!(kept(&validity::check(Some(&star), need - 1)), None, "the checks fit");
        let r = repaired(&star);
        let Geometry::Polygon(parts) = &r.geometry else { panic!("a polygon") };
        assert_eq!((parts.len(), parts[0].len(), parts[0][0].len()), (1, 1, 11), "the star's outline, its centre filled");
        assert!(matches!(repair_with(Some(&star), need, &|_| true, &|_| true), Outcome::Repaired(_)));
        assert_eq!(repair_with(Some(&star), need - 1, &|_| true, &|_| true), Outcome::Kept(Kept::Work));
        // The browser is asked for the overlay's memory, and refusing it leaves the feature as it was.
        let asked = std::cell::Cell::new(0);
        let r = repair_with(Some(&star), validity::MAX_WORK, &|_| true, &|need| {
            asked.set(need);
            false
        });
        assert_eq!(r, Outcome::Kept(Kept::Memory));
        assert_eq!(asked.get(), (6 + 2 * 5) * OVERLAY_BYTES);
        // Without an overlay nothing is asked of the browser.
        let unclosed = poly(&[&[(0., 0.), (10., 0.), (10., 10.), (0., 10.)]]);
        assert!(matches!(repair(Some(&unclosed), &|_| false), Outcome::Repaired(_)));
    }

    /// A star of 2001 points, each joined to the one 1000 on: about 2 million crossings, which the
    /// checks find within the bound (they count a ring's crossings once), but which the overlay
    /// would cut into 4 million vertices.
    #[test]
    fn crossings_are_charged_to_the_overlay() {
        let (k, m) = (2001u32, 1000u32);
        let star: Vec<Coord> = (0..=k)
            .map(|i| {
                let a = std::f64::consts::TAU * f64::from((i * m) % k) / f64::from(k);
                Coord::new(a.cos(), a.sin())
            })
            .collect();
        let g = Geometry::Polygon(vec![vec![star]]);
        let c = validity::check(Some(&g), validity::MAX_WORK);
        assert_eq!(c.codes(), [Code::SelfIntersection], "checked fully");
        assert!(c.contacts > 1_900_000, "{}", c.contacts);
        assert_eq!(fix(&g), Outcome::Kept(Kept::Work));
    }

    /// Every reservation refused in turn: the feature is then left as it was, never dropped,
    /// taken for valid, or written half repaired.
    #[test]
    fn a_reservation_refused_leaves_the_feature_as_it_was() {
        let hole_out: Ring = &[(20., 20.), (21., 20.), (21., 21.), (20., 20.)];
        for g in [
            poly(&[BOWTIE]),
            multi(&[&[SQUARE, hole_out], &[&[(5., 5.), (15., 5.), (15., 15.), (5., 15.), (5., 5.)]]]),
            poly(&[&[(0., 0.), (10., 0.), (10., 0.), (10., 10.)]]),
            Geometry::Line(vec![ring(&[(0., 0.), (1., 1.), (1., 1.)])]),
        ] {
            let full = repaired(&g);
            let tried = std::cell::Cell::new(0usize);
            let _ = repair_with(Some(&g), validity::MAX_WORK, &|_| tried.replace(tried.get() + 1) < usize::MAX, &|_| true);
            let n = tried.get();
            assert!(n >= 2, "{g:?}");
            for k in 0..n {
                let tried = std::cell::Cell::new(0usize);
                let allow = |_| {
                    let i = tried.get();
                    tried.set(i + 1);
                    i != k
                };
                assert_eq!(
                    repair_with(Some(&g), validity::MAX_WORK, &allow, &|_| true),
                    Outcome::Kept(Kept::Memory),
                    "{g:?}, refusing {k}"
                );
            }
            assert_eq!(repair_with(Some(&g), validity::MAX_WORK, &|_| true, &|_| true), Outcome::Repaired(full));
        }
    }

    #[test]
    fn a_feature_the_overlay_can_not_represent_is_left_as_it_was() {
        // A bowtie a hundred-billionth of the smallest normal float across: no grid holds it.
        let tiny: Vec<(f64, f64)> = BOWTIE.iter().map(|&(x, y)| (x * 1e-310, y * 1e-310)).collect();
        let g = poly(&[&tiny]);
        assert_eq!(validity::check(Some(&g), validity::MAX_WORK).codes(), [Code::SelfIntersection]);
        assert_eq!(fix(&g), Outcome::Kept(Kept::Failed));
    }

    #[test]
    fn a_repair_that_is_not_valid_is_not_kept() {
        let bad = Repaired { geometry: poly(&[BOWTIE]), codes: vec![Code::SelfIntersection], dropped: 0, moved: None };
        assert_eq!(verified(bad, validity::MAX_WORK, &|_| true), Outcome::Kept(Kept::Failed));
        let busy = Repaired { geometry: poly(&[BOWTIE]), codes: vec![], dropped: 0, moved: None };
        assert_eq!(verified(busy, 0, &|_| true), Outcome::Kept(Kept::Work));
    }
}
