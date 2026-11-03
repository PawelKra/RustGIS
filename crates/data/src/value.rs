//! Attribute values and field definitions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Integer,
    Double,
    Text,
    Boolean,
}

impl FieldType {
    pub fn label(self) -> &'static str {
        match self {
            FieldType::Integer => "Long",
            FieldType::Double => "Double",
            FieldType::Text => "Text",
            FieldType::Boolean => "Boolean",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, FieldType::Integer | FieldType::Double)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// Display name (the field alias).
    #[serde(default)]
    pub alias: Option<String>,
}

impl Field {
    pub fn new(name: impl Into<String>, ty: FieldType) -> Self {
        Field { name: name.into(), ty, alias: None }
    }
    pub fn display(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Value {
    #[default]
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) if f.is_finite() => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Text(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    /// Converts a JSON value to an attribute value.
    pub fn from_json(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => n.as_f64().map(Value::Float).unwrap_or(Value::Null),
            },
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
            Value::Text(s) => s.clone().into(),
            Value::Bool(b) => (*b).into(),
        }
    }
    /// Parses user text into a value of `ty` (empty → Null).
    pub fn parse(text: &str, ty: FieldType) -> Value {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("<null>") {
            return Value::Null;
        }
        match ty {
            FieldType::Integer => {
                t.parse::<i64>().map(Value::Int).or_else(|_| t.parse::<f64>().map(|f| Value::Int(f as i64))).unwrap_or(Value::Null)
            }
            FieldType::Double => t.parse::<f64>().map(Value::Float).unwrap_or(Value::Null),
            FieldType::Boolean => Value::Bool(matches!(t.to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "y")),
            FieldType::Text => Value::Text(t.to_string()),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "<Null>"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(v) => {
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    write!(f, "{v:.0}")
                } else {
                    write!(f, "{}", (v * 1e6).round() / 1e6)
                }
            }
            Value::Text(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{}", if *b { "True" } else { "False" }),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        match (self.as_f64(), other.as_f64()) {
            (Some(a), Some(b)) if !matches!(self, Value::Text(_)) || !matches!(other, Value::Text(_)) => a.partial_cmp(&b),
            _ => match (self, other) {
                (Value::Null, Value::Null) => Some(Ordering::Equal),
                (Value::Null, _) => Some(Ordering::Less),
                (_, Value::Null) => Some(Ordering::Greater),
                _ => Some(self.to_string().to_lowercase().cmp(&other.to_string().to_lowercase())),
            },
        }
    }
}

/// A Unique Values class's value, given as its text, read as a number: a whole number exactly
/// (past 2^53 too), any other as a decimal.
#[derive(Clone, Copy)]
enum ClassNumber {
    Whole(i128),
    Decimal(f64),
}

fn class_number(text: &str) -> Option<ClassNumber> {
    match text.parse::<i128>() {
        Ok(i) => Some(ClassNumber::Whole(i)),
        Err(_) => text.parse::<f64>().ok().map(ClassNumber::Decimal),
    }
}

/// Orders two class numbers by value, exactly: NaN after every other number.
fn cmp_class_numbers(a: ClassNumber, b: ClassNumber) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // A whole number against a decimal: as decimals, and where they're equal (as a whole number
    // past 2^53 can be to the decimal it rounds to), exactly. A decimal equal to a whole
    // number's is itself whole and within i128, so the cast is exact.
    let whole_to_decimal = |i: i128, f: f64| match (i as f64).partial_cmp(&f) {
        Some(Ordering::Equal) => i.cmp(&(f as i128)),
        Some(o) => o,
        None => Ordering::Less,
    };
    match (a, b) {
        (ClassNumber::Whole(x), ClassNumber::Whole(y)) => x.cmp(&y),
        (ClassNumber::Whole(x), ClassNumber::Decimal(y)) => whole_to_decimal(x, y),
        (ClassNumber::Decimal(x), ClassNumber::Whole(y)) => whole_to_decimal(y, x).reverse(),
        (ClassNumber::Decimal(x), ClassNumber::Decimal(y)) => match (x.is_nan(), y.is_nan()) {
            (false, false) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            (nan_x, nan_y) => nan_x.cmp(&nan_y),
        },
    }
}

/// Whether a class value's text keeps Unique Values classes from being ordered by number: text
/// that reads as no number, other than the empty text and `<Null>` (a missing value's).
pub fn orders_as_text(text: &str) -> bool {
    !text.is_empty() && text != "<Null>" && class_number(text).is_none()
}

/// Whether Unique Values classes with these values, given as their text, are ordered by number:
/// when one reads as a number and every other does too, or is the empty text or `<Null>`. A
/// numeric field's values always are; a text field's when it holds only numbers, such as codes.
pub fn numeric_classes<'a>(values: impl IntoIterator<Item = &'a str>) -> bool {
    let mut number = false;
    for v in values {
        if orders_as_text(v) {
            return false;
        }
        number |= !v.is_empty() && v != "<Null>";
    }
    number
}

/// Orders two Unique Values classes' values, given as their text, when [`numeric_classes`]: by
/// number (9 before 10), then the rest (the empty text, `<Null>`) after them by text. Values equal
/// in number are ordered by text (1 before 1.0), so this is a total order on texts.
pub fn cmp_numeric_classes(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (class_number(a), class_number(b)) {
        (Some(x), Some(y)) => cmp_class_numbers(x, y).then_with(|| a.cmp(b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// What Unique Values leaves out of its classes: how many kept features with a geometry have a
/// value past the first it keeps (drawn as `<all other values>`), and whether those first were
/// taken by number ([`numeric_classes`] over every value) or by text. The classes kept can all be
/// numbers though they were taken by text, past a text value that sorts after them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PastClasses {
    pub features: usize,
    pub by_number: bool,
}

/// Sorts Unique Values classes' values, given as their text: by number when
/// [`numeric_classes`], else by text.
pub fn sort_class_values(values: &mut [String]) {
    if numeric_classes(values.iter().map(String::as_str)) {
        values.sort_by(|a, b| cmp_numeric_classes(a, b));
    } else {
        values.sort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        sort_class_values(&mut v);
        v
    }

    #[test]
    fn classes_are_ordered_by_number_when_every_value_reads_as_one() {
        assert_eq!(sorted(&["100", "9", "10"]), ["9", "10", "100"]);
        assert_eq!(sorted(&["10", "2.5", "-1"]), ["-1", "2.5", "10"]);
        // As a field's display text writes them: a large decimal without an exponent.
        assert_eq!(sorted(&["100000000000000000000", "3e2", "250"]), ["250", "3e2", "100000000000000000000"]);
        assert_eq!(sorted(&[&i64::MAX.to_string(), &i64::MIN.to_string(), "0"]), [i64::MIN.to_string(), "0".into(), i64::MAX.to_string()]);
    }

    #[test]
    fn a_text_among_the_values_keeps_text_order() {
        assert_eq!(sorted(&["b", "A", "10", "9"]), ["10", "9", "A", "b"]);
        assert_eq!(sorted(&["10", "9", "1 0"]), ["1 0", "10", "9"], "a number with a space is text");
        assert!(!numeric_classes(["9", "x"]));
    }

    #[test]
    fn missing_values_go_after_the_numbers() {
        assert_eq!(sorted(&["<Null>", "10", "", "9"]), ["9", "10", "", "<Null>"]);
        assert!(numeric_classes(["<Null>", "5"]));
        // With no number, nothing to order by number.
        assert!(!numeric_classes(["<Null>", ""]));
        assert!(!numeric_classes([]));
    }

    #[test]
    fn whole_numbers_past_2_53_are_ordered_exactly() {
        // 2^53 + 1 and 2^53 are one decimal; as whole numbers they differ.
        assert_eq!(sorted(&["9007199254740993", "9007199254740992"]), ["9007199254740992", "9007199254740993"]);
        assert_eq!(sorted(&["-9007199254740992", "-9007199254740993"]), ["-9007199254740993", "-9007199254740992"]);
        // A decimal that reads as 2^53 is between them.
        assert_eq!(
            sorted(&["9007199254740993", "9007199254740992.5", "9007199254740991"]),
            ["9007199254740991", "9007199254740992.5", "9007199254740993"]
        );
        // Below zero, where their texts order the other way.
        assert_eq!(sorted(&["-9007199254740992.5", "-9007199254740993"]), ["-9007199254740993", "-9007199254740992.5"]);
        assert_eq!(cmp_numeric_classes("9007199254740992.5", "9007199254740993"), std::cmp::Ordering::Less);
        assert_eq!(cmp_numeric_classes("9007199254740993", "9007199254740992.5"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn numbers_equal_in_value_and_not_a_number_have_a_place() {
        // Equal in number: by text, so the order is total and repeatable.
        assert_eq!(sorted(&["1.0", "1"]), ["1", "1.0"]);
        assert_eq!(sorted(&["0", "-0"]), ["-0", "0"]);
        // Infinities in their place, NaN after every number and before missing values.
        assert_eq!(sorted(&["NaN", "inf", "<Null>", "0", "-inf"]), ["-inf", "0", "inf", "NaN", "<Null>"]);
    }

    #[test]
    fn the_numeric_order_is_total() {
        let values = ["NaN", "-inf", "inf", "1", "1.0", "-0", "0", "9007199254740993", "9007199254740992.5", "2.5", "", "<Null>"];
        for a in values {
            assert_eq!(cmp_numeric_classes(a, a), std::cmp::Ordering::Equal, "{a}");
            for b in values {
                assert_eq!(cmp_numeric_classes(a, b), cmp_numeric_classes(b, a).reverse(), "{a} {b}");
                if a != b {
                    assert_ne!(cmp_numeric_classes(a, b), std::cmp::Ordering::Equal, "{a} {b}");
                }
                for c in values {
                    if cmp_numeric_classes(a, b).is_lt() && cmp_numeric_classes(b, c).is_lt() {
                        assert!(cmp_numeric_classes(a, c).is_lt(), "{a} < {b} < {c}");
                    }
                }
            }
        }
    }
}
