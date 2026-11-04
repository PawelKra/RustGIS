//! Check Geometry: a new layer of the features whose geometry breaks the OGC Simple Features rules,
//! each with what is wrong ([`crate::validity`]), in stored coordinates.
use super::*;
use crate::validity::{self, Code};

const TOOL: &str = "Check Geometry";
/// The most problems one feature's `detail` describes; the rest are counted.
const MAX_DETAILS: usize = 20;

/// Writes the checked features (the selected ones, or all) that have problems, as stored, with
/// their attributes and two text fields: the problem codes and their description. A geometry
/// with non-finite coordinates is written without geometry, so it can't spoil the layer's extent.
pub(super) fn run(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("{TOOL} needs a layer with geometry: '{}' is a table", ds.name));
    }
    let mut rows = rows(inp, p, "input", ds);
    rows.sort_unstable();
    rows.dedup();
    let width = ds.fields.len();
    let mut out = copy_schema(ds, out_name(p, format!("{}_check", ds.name)), ds.geometry_type);
    out.crs = ds.crs;
    out.crs_incomplete = ds.crs_incomplete;
    out.zm_dropped = ds.zm_dropped;
    let mut notes = Vec::new();
    for base in ["problem", "detail"] {
        let name = unique_field(&out, base);
        if name != base {
            notes.push(format!("{TOOL}: '{}' already has a field named {base}, so the output's {base} field is {name}", ds.name));
        }
        out.fields.push(Field::new(&name, FieldType::Text));
    }

    // Features with each code, in `Code::ALL` order.
    let mut counts = [0usize; Code::ALL.len()];
    let (mut found, mut unrelated, mut extra) = (0usize, 0usize, 0usize);
    crate::control::begin(rows.len());
    for (i, row) in rows.iter().enumerate() {
        crate::control::check()?;
        crate::control::reached(i);
        let Some(f) = ds.features.get(*row) else { continue };
        let checked = validity::check(f.geometry.as_ref(), validity::MAX_WORK);
        unrelated += usize::from(checked.unrelated);
        let codes = checked.codes();
        if codes.is_empty() {
            continue;
        }
        found += 1;
        for code in &codes {
            if let Some(n) = Code::ALL.iter().position(|c| c == code).and_then(|k| counts.get_mut(k)) {
                *n += 1;
            }
        }
        extra += usize::from(f.attrs.len() > width);
        let mut attrs: Vec<Value> = f.attrs.iter().take(width).cloned().collect();
        attrs.resize(width, Value::Null);
        attrs.push(Value::Text(codes.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")));
        attrs.push(Value::Text(details(&checked.problems)));
        let geometry = if codes.contains(&Code::NonFiniteCoordinate) { None } else { f.geometry.clone() };
        // The input's id, so a row leads back to its feature.
        out.features.push(Feature { id: f.id, geometry, attrs });
    }

    let mut messages = vec![summary(found, rows.len(), &counts)];
    let count = |code: Code| Code::ALL.iter().position(|c| *c == code).and_then(|k| counts.get(k)).copied().unwrap_or(0);
    let non_finite = count(Code::NonFiniteCoordinate);
    if non_finite > 0 {
        messages.push(format!(
            "{TOOL}: {} with non-finite coordinates {} written without geometry",
            features(non_finite),
            if non_finite == 1 { "is" } else { "are" }
        ));
    }
    let complex = count(Code::TooComplex);
    if complex > 0 {
        messages.push(format!(
            "{TOOL}: {} could not be checked fully for crossings and overlaps (too complex for the work limit); {} in the output as too_complex",
            features(complex),
            if complex == 1 { "it is" } else { "they are" }
        ));
    }
    if unrelated > 0 {
        messages.push(format!(
            "{TOOL}: the rings or parts of {} weren't checked against each other, because some of their rings have too few points, non-finite coordinates or cross themselves",
            features(unrelated)
        ));
    }
    if extra > 0 {
        messages.push(format!(
            "{TOOL}: {} had more attribute values than the layer has fields; the extra values aren't in the output",
            features(extra)
        ));
    }
    messages.extend(notes);
    Ok(ToolResult { output: Output::Layer(out), messages })
}

/// "1 feature", "2 features".
fn features(n: usize) -> String {
    format!("{n} feature{}", if n == 1 { "" } else { "s" })
}

/// The run's result in one line, with the features having each code.
fn summary(found: usize, checked: usize, counts: &[usize]) -> String {
    if checked == 0 {
        return format!("{TOOL}: no features to check");
    }
    if found == 0 {
        return format!("{TOOL}: no problems found in {}", features(checked));
    }
    let by_code: Vec<String> = Code::ALL.iter().zip(counts).filter(|(_, n)| **n > 0).map(|(c, n)| format!("{}: {n}", c.as_str())).collect();
    format!("{TOOL}: {found} of {} {} problems ({})", features(checked), if found == 1 { "has" } else { "have" }, by_code.join(", "))
}

/// The descriptions of a feature's problems, joined with "; ": the first [`MAX_DETAILS`], then how
/// many more there are.
fn details(problems: &[validity::Problem]) -> String {
    let mut parts: Vec<String> = problems.iter().take(MAX_DETAILS).map(validity::Problem::describe).collect();
    let more = problems.len().saturating_sub(MAX_DETAILS);
    if more > 0 {
        parts.push(format!("and {more} more problem{}", if more == 1 { "" } else { "s" }));
    }
    parts.join("; ")
}
