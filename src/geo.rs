//! Where a node sits on the world map.
//!
//! The control plane sends `country` (ISO 3166-1 alpha-2) and a display `city`,
//! and nothing else — no coordinates. So the placement is looked up here: the
//! city first, the country centroid when the city is unknown. A node that
//! matches neither is left off the map rather than dropped somewhere wrong; the
//! list still carries it.
//!
//! If the control plane ever starts sending latitude and longitude, delete this
//! and use them.

/// Latitude band the map covers. Antarctica is cut off, so is the very top of
/// Greenland. Must match `ui/assets/world-dots.svg`.
pub const LAT_TOP: f64 = 83.0;
pub const LAT_BOTTOM: f64 = -56.0;

/// Normalised position inside the map image, both in 0..1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Spot {
    pub nx: f32,
    pub ny: f32,
}

pub fn locate(country: &str, city: &str) -> Option<Spot> {
    let country = normalise(country);
    let city = normalise(city);

    let coords = CITIES
        .iter()
        .find(|(c, name, ..)| *c == country && *name == city)
        .map(|(_, _, lat, lon)| (*lat, *lon))
        .or_else(|| {
            COUNTRIES
                .iter()
                .find(|(c, ..)| *c == country)
                .map(|(_, lat, lon)| (*lat, *lon))
        })?;

    Some(project(coords.0, coords.1))
}

pub fn project(lat: f64, lon: f64) -> Spot {
    let nx = (lon + 180.0) / 360.0;
    let ny = (LAT_TOP - lat) / (LAT_TOP - LAT_BOTTOM);
    Spot {
        nx: nx.clamp(0.0, 1.0) as f32,
        ny: ny.clamp(0.0, 1.0) as f32,
    }
}

/// Lowercases and folds the accents that separate `Montréal` from `montreal`,
/// so the table can be written one way and still match what the service sends.
///
/// It is also the sort key for country names. Comparing them byte by byte puts
/// `États-Unis` after `Suède`, because every accented letter outranks the whole
/// ASCII alphabet — which is not where a reader looks for it.
pub fn fold(value: &str) -> String {
    normalise(value)
}

fn normalise(value: &str) -> String {
    // Unicode lowercasing, not ASCII: `to_ascii_lowercase` leaves `É` alone, and
    // the folding below only names lowercase letters, so an accented capital
    // would sail through unfolded.
    value
        .trim()
        .to_lowercase()
        .chars()
        .flat_map(|c| {
            let folded = match c {
                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'í' | 'ì' | 'î' | 'ï' => 'i',
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
                'ú' | 'ù' | 'û' | 'ü' => 'u',
                'ç' => 'c',
                'ñ' => 'n',
                'ý' | 'ÿ' => 'y',
                other => other,
            };
            Some(folded)
        })
        .collect()
}

/// (country, city, latitude, longitude). Cities that actually carry VPN exits.
#[rustfmt::skip]
const CITIES: &[(&str, &str, f64, f64)] = &[
    ("al", "tirana", 41.33, 19.82),
    ("ar", "buenos aires", -34.60, -58.38),
    ("at", "vienna", 48.21, 16.37),
    ("au", "sydney", -33.87, 151.21),
    ("au", "melbourne", -37.81, 144.96),
    ("au", "brisbane", -27.47, 153.03),
    ("au", "perth", -31.95, 115.86),
    ("au", "adelaide", -34.93, 138.60),
    ("ba", "sarajevo", 43.86, 18.41),
    ("be", "brussels", 50.85, 4.35),
    ("bg", "sofia", 42.70, 23.32),
    ("br", "sao paulo", -23.55, -46.63),
    ("ca", "toronto", 43.65, -79.38),
    ("ca", "montreal", 45.50, -73.57),
    ("ca", "vancouver", 49.28, -123.12),
    ("ca", "calgary", 51.05, -114.07),
    ("ch", "zurich", 47.38, 8.54),
    ("ch", "geneva", 46.20, 6.14),
    ("cl", "santiago", -33.45, -70.67),
    ("co", "bogota", 4.71, -74.07),
    ("cy", "nicosia", 35.19, 33.38),
    ("cz", "prague", 50.08, 14.44),
    ("de", "frankfurt", 50.11, 8.68),
    ("de", "berlin", 52.52, 13.40),
    ("de", "dusseldorf", 51.23, 6.78),
    ("de", "munich", 48.14, 11.58),
    ("dk", "copenhagen", 55.68, 12.57),
    ("ee", "tallinn", 59.44, 24.75),
    ("es", "madrid", 40.42, -3.70),
    ("es", "barcelona", 41.39, 2.17),
    ("es", "valencia", 39.47, -0.38),
    ("fi", "helsinki", 60.17, 24.94),
    ("fr", "paris", 48.86, 2.35),
    ("fr", "marseille", 43.30, 5.37),
    ("fr", "bordeaux", 44.84, -0.58),
    ("fr", "lyon", 45.76, 4.83),
    ("gb", "london", 51.51, -0.13),
    ("gb", "manchester", 53.48, -2.24),
    ("gb", "glasgow", 55.86, -4.25),
    ("ge", "tbilisi", 41.72, 44.78),
    ("gr", "athens", 37.98, 23.73),
    ("hk", "hong kong", 22.32, 114.17),
    ("hr", "zagreb", 45.81, 15.98),
    ("hu", "budapest", 47.50, 19.04),
    ("id", "jakarta", -6.21, 106.85),
    ("ie", "dublin", 53.35, -6.26),
    ("il", "tel aviv", 32.08, 34.78),
    ("in", "mumbai", 19.08, 72.88),
    ("in", "delhi", 28.61, 77.21),
    ("in", "chennai", 13.08, 80.27),
    ("is", "reykjavik", 64.15, -21.94),
    ("it", "milan", 45.46, 9.19),
    ("it", "rome", 41.90, 12.50),
    ("it", "naples", 40.85, 14.27),
    ("it", "palermo", 38.12, 13.36),
    ("jp", "tokyo", 35.68, 139.69),
    ("jp", "osaka", 34.69, 135.50),
    ("kr", "seoul", 37.57, 126.98),
    ("kz", "almaty", 43.24, 76.89),
    ("lt", "vilnius", 54.69, 25.28),
    ("lu", "luxembourg", 49.61, 6.13),
    ("lv", "riga", 56.95, 24.11),
    ("md", "chisinau", 47.01, 28.86),
    ("mk", "skopje", 41.99, 21.43),
    ("mx", "mexico city", 19.43, -99.13),
    ("mx", "queretaro", 20.59, -100.39),
    ("my", "kuala lumpur", 3.14, 101.69),
    ("ng", "lagos", 6.52, 3.38),
    ("nl", "amsterdam", 52.37, 4.90),
    ("nl", "rotterdam", 51.92, 4.48),
    ("nl", "the hague", 52.08, 4.31),
    ("no", "oslo", 59.91, 10.75),
    ("no", "stavanger", 58.97, 5.73),
    ("nz", "auckland", -36.85, 174.76),
    ("pe", "lima", -12.05, -77.04),
    ("ph", "manila", 14.60, 120.98),
    ("pl", "warsaw", 52.23, 21.01),
    ("pt", "lisbon", 38.72, -9.14),
    ("ro", "bucharest", 44.43, 26.10),
    ("rs", "belgrade", 44.79, 20.45),
    ("se", "stockholm", 59.33, 18.07),
    ("se", "gothenburg", 57.71, 11.97),
    ("se", "malmo", 55.60, 13.00),
    ("sg", "singapore", 1.35, 103.82),
    ("si", "ljubljana", 46.06, 14.51),
    ("sk", "bratislava", 48.15, 17.11),
    ("th", "bangkok", 13.76, 100.50),
    ("tr", "istanbul", 41.01, 28.98),
    ("tw", "taipei", 25.03, 121.57),
    ("ua", "kyiv", 50.45, 30.52),
    ("ae", "dubai", 25.20, 55.27),
    ("us", "new york", 40.71, -74.01),
    ("us", "los angeles", 34.05, -118.24),
    ("us", "chicago", 41.88, -87.63),
    ("us", "dallas", 32.78, -96.80),
    ("us", "miami", 25.76, -80.19),
    ("us", "seattle", 47.61, -122.33),
    ("us", "denver", 39.74, -104.98),
    ("us", "atlanta", 33.75, -84.39),
    ("us", "phoenix", 33.45, -112.07),
    ("us", "salt lake city", 40.76, -111.89),
    ("us", "san jose", 37.34, -121.89),
    ("us", "ashburn", 39.04, -77.49),
    ("us", "houston", 29.76, -95.37),
    ("us", "boston", 42.36, -71.06),
    ("us", "las vegas", 36.17, -115.14),
    ("us", "detroit", 42.33, -83.05),
    ("us", "secaucus", 40.79, -74.06),
    ("vn", "hanoi", 21.03, 105.85),
    ("za", "johannesburg", -26.20, 28.05),
    ("za", "cape town", -33.92, 18.42),
];

/// `code<TAB>English name`, one per line, built from the flag-icons set plus the
/// spellings this control plane actually uses. Several lines may share a code:
/// it answers "which country is this called", not "what is this country called".
static COUNTRY_NAMES: &str = include_str!("../ui/assets/countries.tsv");

/// Settles which country a record belongs to.
///
/// The control plane sends both an ISO-2 `country_code` and a full English
/// `country`, and leaves the code empty on some records while still naming them.
/// So the code is used when it is there, and the name resolves it when it is
/// not — otherwise those exits would have no flag, no position and no country to
/// sit under.
pub fn resolve_code(country_code: &str, country_name: &str) -> Option<String> {
    let code = country_code.trim().to_ascii_lowercase();
    if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Some(code);
    }

    let wanted = fold(country_name);
    if wanted.is_empty() {
        return None;
    }
    COUNTRY_NAMES.lines().find_map(|line| {
        let (code, name) = line.split_once('\t')?;
        (fold(name) == wanted).then(|| code.to_ascii_lowercase())
    })
}

/// The country's name for a list header. The control plane sends a code, and a
/// code is not a label.
pub fn english_name(code: &str) -> Option<&'static str> {
    let code = code.trim().to_ascii_lowercase();
    COUNTRY_NAMES
        .lines()
        .find_map(|line| line.split_once('\t').filter(|(c, _)| *c == code))
        .map(|(_, name)| name)
}

/// Country centroids, used when the city is not in the table above.
#[rustfmt::skip]
const COUNTRIES: &[(&str, f64, f64)] = &[
    ("ae", 23.42, 53.85), ("al", 41.15, 20.17), ("am", 40.07, 45.04),
    ("ar", -38.42, -63.62), ("at", 47.52, 14.55), ("au", -25.27, 133.78),
    ("az", 40.14, 47.58), ("ba", 43.92, 17.68), ("bd", 23.68, 90.36),
    ("be", 50.50, 4.47), ("bg", 42.73, 25.49), ("br", -14.24, -51.93),
    ("by", 53.71, 27.95), ("ca", 56.13, -106.35), ("ch", 46.82, 8.23),
    ("cl", -35.68, -71.54), ("cn", 35.86, 104.20), ("co", 4.57, -74.30),
    ("cr", 9.75, -83.75), ("cy", 35.13, 33.43), ("cz", 49.82, 15.47),
    ("de", 51.17, 10.45), ("dk", 56.26, 9.50), ("do", 18.74, -70.16),
    ("dz", 28.03, 1.66), ("ec", -1.83, -78.18), ("ee", 58.60, 25.01),
    ("eg", 26.82, 30.80), ("es", 40.46, -3.75), ("fi", 61.92, 25.75),
    ("fr", 46.23, 2.21), ("gb", 55.38, -3.44), ("ge", 42.32, 43.36),
    ("gr", 39.07, 21.82), ("gt", 15.78, -90.23), ("hk", 22.32, 114.17),
    ("hr", 45.10, 15.20), ("hu", 47.16, 19.50), ("id", -0.79, 113.92),
    ("ie", 53.41, -8.24), ("il", 31.05, 34.85), ("in", 20.59, 78.96),
    ("iq", 33.22, 43.68), ("is", 64.96, -19.02), ("it", 41.87, 12.57),
    ("jp", 36.20, 138.25), ("ke", -0.02, 37.91), ("kh", 12.57, 104.99),
    ("kr", 35.91, 127.77), ("kz", 48.02, 66.92), ("lb", 33.85, 35.86),
    ("lt", 55.17, 23.88), ("lu", 49.82, 6.13), ("lv", 56.88, 24.60),
    ("ma", 31.79, -7.09), ("md", 47.41, 28.37), ("me", 42.71, 19.37),
    ("mk", 41.61, 21.75), ("mt", 35.94, 14.38), ("mx", 23.63, -102.55),
    ("my", 4.21, 101.98), ("ng", 9.08, 8.68), ("nl", 52.13, 5.29),
    ("no", 60.47, 8.47), ("np", 28.39, 84.12), ("nz", -40.90, 174.89),
    ("pa", 8.54, -80.78), ("pe", -9.19, -75.02), ("ph", 12.88, 121.77),
    ("pk", 30.38, 69.35), ("pl", 51.92, 19.15), ("pt", 39.40, -8.22),
    ("py", -23.44, -58.44), ("ro", 45.94, 24.97), ("rs", 44.02, 21.01),
    ("ru", 55.75, 37.62), ("sa", 23.89, 45.08), ("se", 60.13, 18.64),
    ("sg", 1.35, 103.82), ("si", 46.15, 14.99), ("sk", 48.67, 19.70),
    ("th", 15.87, 100.99), ("tn", 33.89, 9.54), ("tr", 38.96, 35.24),
    ("tw", 23.70, 120.96), ("ua", 48.38, 31.17), ("us", 39.83, -98.58),
    ("uy", -32.52, -55.77), ("uz", 41.38, 64.59), ("ve", 6.42, -66.59),
    ("vn", 14.06, 108.28), ("za", -30.56, 22.94),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn city_beats_country() {
        let paris = locate("fr", "Paris").unwrap();
        let france = locate("fr", "Nowhere").unwrap();
        assert!(paris != france);
    }

    #[test]
    fn folds_accents_and_case() {
        // Spellings the service actually sends: São Paulo, Hénin-Beaumont,
        // Rajbāri. The table is written unaccented and has to match anyway.
        assert!(locate("BR", "São Paulo").is_some());
        assert_eq!(locate("ca", "montreal"), locate("CA", "Montréal"));
        // Accented capitals too, which ASCII lowercasing used to walk straight
        // past.
        assert_eq!(locate("ca", "MONTRÉAL"), locate("ca", "montreal"));
        assert_eq!(fold("SÃO PAULO"), "sao paulo");
    }

    #[test]
    fn unknown_country_is_not_placed() {
        assert!(locate("zz", "Atlantis").is_none());
    }

    #[test]
    fn a_code_wins_and_a_name_covers_for_it() {
        // The usual case: the service sends the code, in upper case.
        assert_eq!(resolve_code("FR", "France").as_deref(), Some("fr"));
        // And the records where it leaves the code empty.
        assert_eq!(resolve_code("", "France").as_deref(), Some("fr"));
        assert_eq!(resolve_code("   ", "Japan").as_deref(), Some("jp"));
    }

    #[test]
    fn resolves_the_spellings_this_service_uses() {
        // Every one of these differs from the flag set's own name, and each is
        // taken from a real /api/v1/exits response.
        for (name, code) in [
            ("United States", "us"),
            ("Turkey", "tr"),
            ("Czechia", "cz"),
            ("The Netherlands", "nl"),
            ("Brunei", "bn"),
            ("Congo (DRC)", "cd"),
        ] {
            assert_eq!(resolve_code("", name).as_deref(), Some(code), "{name}");
        }
    }

    #[test]
    fn refuses_what_it_cannot_place() {
        assert_eq!(resolve_code("", "Atlantis"), None);
        assert_eq!(resolve_code("", ""), None);
        // Three letters is not alpha-2, and must not be taken for it.
        assert_eq!(resolve_code("FRA", "Atlantis"), None);
    }

    #[test]
    fn names_a_country_from_the_flag_set() {
        assert_eq!(english_name("de"), Some("Germany"));
        assert_eq!(english_name("af"), Some("Afghanistan"));
        assert_eq!(english_name("zz"), None);
    }

    #[test]
    fn folding_sorts_accented_names_where_a_reader_looks() {
        // The service spells some countries with accents — Curaçao, Åland,
        // Côte d'Ivoire — and byte order buries every one of them past Z.
        let mut names = ["Sweden", "Åland Islands", "Switzerland", "Canada"];
        names.sort_by_key(|name| fold(name));
        assert_eq!(
            names,
            ["Åland Islands", "Canada", "Sweden", "Switzerland"]
        );

        let mut raw = ["Sweden", "Åland Islands", "Canada"];
        raw.sort();
        assert_eq!(raw.last(), Some(&"Åland Islands"));
    }

    #[test]
    fn corners_project_inside_the_image() {
        let top_left = project(LAT_TOP, -180.0);
        assert!(top_left.nx.abs() < 1e-6 && top_left.ny.abs() < 1e-6);
        let bottom_right = project(LAT_BOTTOM, 180.0);
        assert!((bottom_right.nx - 1.0).abs() < 1e-6);
        assert!((bottom_right.ny - 1.0).abs() < 1e-6);
    }
}
