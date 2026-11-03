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

/// A Unique Values class's value, given as its text, read as a number: a whole number (a sign,
/// then digits, of any length) by its digits, so exactly; any other as a decimal.
#[derive(Clone, Copy, Debug)]
enum ClassNumber {
    /// Below zero or not, and where its digits start in the text, past the sign and any leading
    /// zeros (none left for 0).
    Whole {
        negative: bool,
        start: usize,
    },
    Decimal(f64),
}

fn class_number(text: &str) -> Option<ClassNumber> {
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit()) {
        let digits = unsigned.trim_start_matches('0');
        return Some(ClassNumber::Whole { negative: negative && !digits.is_empty(), start: text.len() - digits.len() });
    }
    text.parse::<f64>().ok().map(ClassNumber::Decimal)
}

/// A whole number's sign and digits (past leading zeros) in `text`.
fn whole_digits(text: &str, negative: bool, start: usize) -> (bool, &str) {
    (negative, text.get(start..).unwrap_or_default())
}

/// Orders two whole numbers given by their sign and digits (without leading zeros).
fn cmp_wholes((negative_a, a): (bool, &str), (negative_b, b): (bool, &str)) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let size = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
    match (negative_a, negative_b) {
        (false, false) => size,
        (true, true) => size.reverse(),
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
    }
}

/// Orders the whole number written `text` (its sign and digits `whole`) against the decimal `f`,
/// exactly: NaN after it.
fn cmp_whole_decimal(text: &str, whole: (bool, &str), f: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if f.is_nan() {
        return Ordering::Less;
    }
    if f.is_infinite() {
        return if f > 0.0 { Ordering::Less } else { Ordering::Greater };
    }
    // As decimals first: rounding keeps order, so where the whole number's decimal is above or
    // below `f`, the whole number is too.
    match text.parse::<f64>().ok().and_then(|w| w.partial_cmp(&f)) {
        Some(Ordering::Equal) | None => {}
        Some(o) => return o,
    }
    // Equal as decimals, `f` is a whole number (one a whole number rounds to): written out in
    // full, its digits are compared exactly (2^53 + 1 against 2^53, i128::MAX against 2^127).
    let full = format!("{f:.0}");
    match class_number(&full) {
        Some(ClassNumber::Whole { negative, start }) => cmp_wholes(whole, whole_digits(&full, negative, start)),
        _ => Ordering::Equal,
    }
}

/// Orders two class numbers, read from the texts `a` and `b`, by value: whole numbers exactly,
/// decimals as `f64`, NaN after every other number. Two decimals' texts that round to the same
/// `f64` compare equal here (and then by text in [`cmp_numeric_classes`]); a numeric field's never
/// do, as its display text reads back as the same `f64`, so only a text field of decimals past
/// 15 or so digits can be ordered by text.
fn cmp_class_numbers(a: &str, x: ClassNumber, b: &str, y: ClassNumber) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (x, y) {
        (ClassNumber::Whole { negative: na, start: sa }, ClassNumber::Whole { negative: nb, start: sb }) => {
            cmp_wholes(whole_digits(a, na, sa), whole_digits(b, nb, sb))
        }
        (ClassNumber::Whole { negative, start }, ClassNumber::Decimal(f)) => cmp_whole_decimal(a, whole_digits(a, negative, start), f),
        (ClassNumber::Decimal(f), ClassNumber::Whole { negative, start }) => {
            cmp_whole_decimal(b, whole_digits(b, negative, start), f).reverse()
        }
        (ClassNumber::Decimal(f), ClassNumber::Decimal(g)) => match (f.is_nan(), g.is_nan()) {
            (false, false) => f.partial_cmp(&g).unwrap_or(Ordering::Equal),
            (nan_f, nan_g) => nan_f.cmp(&nan_g),
        },
    }
}

/// Orders two class values, each a text and the number it reads as, as [`cmp_numeric_classes`].
fn cmp_read_classes(a: &str, x: Option<ClassNumber>, b: &str, y: Option<ClassNumber>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (x, y) {
        (Some(x), Some(y)) => cmp_class_numbers(a, x, b, y).then_with(|| a.cmp(b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Whether a value's text is a missing value's: the empty text or `<Null>`.
fn missing(text: &str) -> bool {
    text.is_empty() || text == "<Null>"
}

/// Whether a class value's text keeps Unique Values classes from being ordered by number: text
/// that reads as no number, other than the empty text and `<Null>` (a missing value's).
pub fn orders_as_text(text: &str) -> bool {
    !missing(text) && class_number(text).is_none()
}

/// [`numeric_classes`] over texts already read as numbers.
fn numeric_read<'a>(values: impl IntoIterator<Item = (&'a str, Option<ClassNumber>)>) -> bool {
    let mut number = false;
    for (text, n) in values {
        if missing(text) {
            continue;
        }
        if n.is_none() {
            return false;
        }
        number = true;
    }
    number
}

/// Whether Unique Values classes with these values, given as their text, are ordered by number:
/// when one reads as a number and every other does too, or is the empty text or `<Null>`. A
/// numeric field's values always are; a text field's when it holds only numbers, such as codes.
pub fn numeric_classes<'a>(values: impl IntoIterator<Item = &'a str>) -> bool {
    numeric_read(values.into_iter().map(|v| (v, class_number(v))))
}

/// Orders two Unique Values classes' values, given as their text, when [`numeric_classes`]: by
/// number (9 before 10), then the rest (the empty text, `<Null>`) after them by text. Values equal
/// in number are ordered by text (1 before 1.0), so this is a total order on texts.
pub fn cmp_numeric_classes(a: &str, b: &str) -> std::cmp::Ordering {
    cmp_read_classes(a, class_number(a), b, class_number(b))
}

/// A class value's text, read as a number once: ordered as [`cmp_numeric_classes`] orders texts,
/// without reading them again on every comparison.
#[derive(Clone, Debug)]
pub struct ClassText {
    text: String,
    number: Option<ClassNumber>,
}

impl ClassText {
    pub fn new(text: String) -> Self {
        let number = class_number(&text);
        ClassText { text, number }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// [`orders_as_text`] for this value.
    pub fn orders_as_text(&self) -> bool {
        !missing(&self.text) && self.number.is_none()
    }

    /// [`numeric_classes`] over these values.
    pub fn numeric<'a>(values: impl IntoIterator<Item = &'a ClassText>) -> bool {
        numeric_read(values.into_iter().map(|c| (c.text.as_str(), c.number)))
    }
}

impl PartialEq for ClassText {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for ClassText {}

impl Ord for ClassText {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        cmp_read_classes(&self.text, self.number, &other.text, other.number)
    }
}

impl PartialOrd for ClassText {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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
pub fn sort_class_values(values: &mut Vec<String>) {
    let mut read: Vec<ClassText> = std::mem::take(values).into_iter().map(ClassText::new).collect();
    if ClassText::numeric(&read) {
        read.sort();
    } else {
        read.sort_by(|a, b| a.text.cmp(&b.text));
    }
    values.extend(read.into_iter().map(ClassText::into_text));
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

    /// Whole numbers of any length are ordered by their digits: past i128 too, and against a
    /// decimal at i128's limit.
    #[test]
    fn whole_numbers_past_i128_are_ordered_exactly() {
        let below_min = "-170141183460469231731687303715884105729";
        let further_below = "-170141183460469231731687303715884105730";
        assert_eq!(sorted(&[below_min, further_below]), [further_below, below_min]);
        let past_max = "170141183460469231731687303715884105729";
        let further_past = "170141183460469231731687303715884105730";
        assert_eq!(sorted(&[further_past, past_max]), [past_max, further_past]);
        // i128::MAX is below 2^127, which reads as one decimal with it.
        let max = i128::MAX.to_string();
        assert_eq!(sorted(&["1.7014118346046923e38", &max]), [max.as_str(), "1.7014118346046923e38"]);
        assert_eq!(
            sorted(&["170141183460469231731687303715884105728.0", past_max]),
            ["170141183460469231731687303715884105728.0", past_max]
        );
        // Hundreds of digits: above every decimal but infinity.
        let huge = format!("1{}", "0".repeat(400));
        let below_huge = format!("-{huge}");
        assert_eq!(
            sorted(&["inf", &huge, "1e308", &below_huge, "-inf", "-1e308"]),
            ["-inf", below_huge.as_str(), "-1e308", "1e308", huge.as_str(), "inf"]
        );
        // A large float's display text is a whole number in full (its shortest digits, then
        // zeros): neighbouring floats' texts are in the floats' order.
        for v in [1e300, -1e300, 1.2345678901234568e20, 9.007199254740994e15] {
            let (a, b) = (Value::Float(v).to_string(), Value::Float(v.next_up()).to_string());
            assert!(matches!(class_number(&a), Some(ClassNumber::Whole { .. })), "{a}");
            assert_eq!(cmp_numeric_classes(&a, &b), std::cmp::Ordering::Less, "{a} {b}");
        }
        // Signs and leading zeros: one number, ordered by text.
        assert_eq!(sorted(&["7", "007", "+7", "-0", "0", "-00", "+0"]), ["+0", "-0", "-00", "0", "+7", "007", "7"]);
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
        let values = [
            "NaN",
            "-inf",
            "inf",
            "1",
            "1.0",
            "+1",
            "01",
            "-0",
            "0",
            "9007199254740993",
            "9007199254740992.5",
            "-9007199254740993",
            "-9007199254740992.5",
            "170141183460469231731687303715884105727",
            "1.7014118346046923e38",
            "170141183460469231731687303715884105729",
            "-170141183460469231731687303715884105730",
            "2.5",
            "",
            "<Null>",
        ];
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
