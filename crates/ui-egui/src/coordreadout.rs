//! How the pointer's coordinates read in the status bar and in the map's right-click menu:
//! decimal degrees, degrees-minutes-seconds, UTM or MGRS. A click on the status bar readout
//! steps through them in that order (`ui.coordinateFormat`), and the choice is remembered
//! between sessions as the `coordinate_format` preference.

use rustgis_engine::geom::{self, Coord};

use crate::i18n;

/// The preference that keeps the chosen format: its [`CoordReadout::code`].
pub const PREF: &str = "coordinate_format";

/// A coordinate readout format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordReadout {
    /// `12.3456° E  45.6789° N`
    #[default]
    Decimal,
    /// `12°20′44.2″ E  45°40′44.0″ N`
    Dms,
    /// `33U 412345 5523456`: zone and latitude band, easting and northing in metres.
    Utm,
    /// `33U VP 12345 23456`: zone and band, the 100 km square, easting and northing to 1 m.
    Mgrs,
}

impl CoordReadout {
    /// Every format, in the order a click steps through them.
    pub const ALL: [CoordReadout; 4] = [CoordReadout::Decimal, CoordReadout::Dms, CoordReadout::Utm, CoordReadout::Mgrs];

    /// The name commands and the preference use (never translated).
    pub fn code(self) -> &'static str {
        match self {
            CoordReadout::Decimal => "decimal",
            CoordReadout::Dms => "dms",
            CoordReadout::Utm => "utm",
            CoordReadout::Mgrs => "mgrs",
        }
    }

    /// The format a [`code`](Self::code) names, ignoring case and spaces around it.
    pub fn from_code(code: &str) -> Option<Self> {
        let code = code.trim();
        Self::ALL.into_iter().find(|f| f.code().eq_ignore_ascii_case(code))
    }

    /// The format after this one: decimal → DMS → UTM → MGRS → decimal.
    pub fn next(self) -> Self {
        match self {
            CoordReadout::Decimal => CoordReadout::Dms,
            CoordReadout::Dms => CoordReadout::Utm,
            CoordReadout::Utm => CoordReadout::Mgrs,
            CoordReadout::Mgrs => CoordReadout::Decimal,
        }
    }

    /// The format's name, translated in logical order, for filling into [`i18n::fmt`] messages.
    pub fn label(self) -> &'static str {
        match self {
            CoordReadout::Decimal => tl_raw!("decimal degrees"),
            CoordReadout::Dms => tl_raw!("degrees, minutes and seconds"),
            CoordReadout::Utm => tl_raw!("UTM"),
            CoordReadout::Mgrs => tl_raw!("MGRS"),
        }
    }

    /// The place in this format; for UTM and MGRS `None` outside their latitudes (80° S to
    /// 84° N), and for every format `None` for a non-finite place.
    pub fn format(self, c: Coord) -> Option<String> {
        if !c.is_finite() {
            return None;
        }
        match self {
            CoordReadout::Decimal => Some(geom::format_lonlat(c)),
            CoordReadout::Dms => Some(geom::format_dms(c)),
            CoordReadout::Utm => geom::format_utm(c),
            CoordReadout::Mgrs => geom::format_mgrs(c),
        }
    }

    /// The status bar's readout of the pointer: `—` off the map, and a plain statement where UTM
    /// and MGRS have no zone (the polar areas, which use UPS) rather than a wrong grid reference.
    pub fn readout(self, c: Option<Coord>) -> String {
        match c {
            None => "—".to_string(),
            Some(c) if !c.is_finite() => "—".to_string(),
            Some(c) => self.format(c).unwrap_or_else(|| outside_utm(self)),
        }
    }

    /// The status bar readout's hover text: the current format and what a click does.
    pub fn hover(self) -> String {
        i18n::fmt(
            "Coordinates in {format}. Click to switch: decimal degrees, DMS, UTM, MGRS. UTM and MGRS cover 80° S to 84° N.",
            &[("format", self.label())],
        )
    }
}

/// What the readout says where UTM and MGRS have no zone.
fn outside_utm(f: CoordReadout) -> String {
    i18n::fmt("No {format} here: outside the UTM zones (80° S to 84° N)", &[("format", f.label())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_steps_through_every_format_and_back() {
        let mut f = CoordReadout::default();
        assert_eq!(f, CoordReadout::Decimal);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(f);
            f = f.next();
        }
        assert_eq!(seen, CoordReadout::ALL);
        assert_eq!(f, CoordReadout::Decimal);
    }

    #[test]
    fn codes_round_trip() {
        for f in CoordReadout::ALL {
            assert_eq!(CoordReadout::from_code(f.code()), Some(f));
            assert_eq!(CoordReadout::from_code(&format!(" {} ", f.code().to_uppercase())), Some(f));
        }
        assert_eq!(CoordReadout::from_code("ups"), None);
        assert_eq!(CoordReadout::from_code(""), None);
    }

    #[test]
    fn readouts_in_each_format() {
        let p = Coord::new(44.4, 33.3);
        assert_eq!(CoordReadout::Decimal.readout(Some(p)), "44.4000° E  33.3000° N");
        assert_eq!(CoordReadout::Dms.readout(Some(p)), "44°24′00.0″ E  33°18′00.0″ N");
        assert_eq!(CoordReadout::Utm.readout(Some(p)), "38S 444141 3684706");
        assert_eq!(CoordReadout::Mgrs.readout(Some(p)), "38S MB 44140 84706");
        for f in CoordReadout::ALL {
            assert_eq!(f.readout(None), "—");
            assert_eq!(f.readout(Some(Coord::new(f64::NAN, 0.0))), "—");
        }
    }

    #[test]
    fn polar_places_say_they_are_outside_utm() {
        let pole = Coord::new(10.0, 86.0);
        for f in [CoordReadout::Utm, CoordReadout::Mgrs] {
            assert_eq!(f.format(pole), None);
            let r = f.readout(Some(pole));
            assert!(r.contains("outside the UTM zones") && r.contains(f.label()), "{r}");
        }
        // Decimal and DMS read there as anywhere.
        assert_eq!(CoordReadout::Dms.readout(Some(pole)), "10°00′00.0″ E  86°00′00.0″ N");
    }
}
