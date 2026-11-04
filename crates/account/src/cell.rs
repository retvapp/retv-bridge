//! Which ReTV cell (and so which site) Bridge signs in to: retv.app or
//! ru.retv.app. The rule is ReTV Home's (`RelaySite.kt`): a Russian-language
//! computer goes to ru.retv.app only when its clock is set to a Russian time
//! zone, or to no place at all (UTC, Etc/…). The person can switch.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cell {
    World,
    Ru,
}

/// Russia's time zones (tzdata names, aliases included).
const RU_ZONES: [&str; 27] = [
    "Europe/Moscow",
    "Europe/Kaliningrad",
    "Europe/Samara",
    "Europe/Volgograd",
    "Europe/Kirov",
    "Europe/Astrakhan",
    "Europe/Saratov",
    "Europe/Ulyanovsk",
    "W-SU",
    "Asia/Yekaterinburg",
    "Asia/Omsk",
    "Asia/Novosibirsk",
    "Asia/Barnaul",
    "Asia/Tomsk",
    "Asia/Novokuznetsk",
    "Asia/Krasnoyarsk",
    "Asia/Irkutsk",
    "Asia/Chita",
    "Asia/Yakutsk",
    "Asia/Khandyga",
    "Asia/Vladivostok",
    "Asia/Ust-Nera",
    "Asia/Magadan",
    "Asia/Sakhalin",
    "Asia/Srednekolymsk",
    "Asia/Kamchatka",
    "Asia/Anadyr",
];

/// The clock names no place: it was never set to a city.
fn placeless(zone: &str) -> bool {
    matches!(
        zone,
        "" | "UTC" | "GMT" | "UCT" | "Zulu" | "Universal" | "Etc/UTC" | "Etc/GMT"
    ) || zone.starts_with("Etc/")
}

impl Cell {
    /// The cell for a language code (`ru`, `en`, …) and an IANA time zone.
    pub fn pick(lang: &str, zone: &str) -> Cell {
        let ru = lang.to_ascii_lowercase().starts_with("ru");
        if ru && (RU_ZONES.contains(&zone) || placeless(zone)) {
            Cell::Ru
        } else {
            Cell::World
        }
    }

    pub fn parse(s: &str) -> Option<Cell> {
        match s {
            "world" => Some(Cell::World),
            "ru" => Some(Cell::Ru),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Cell::World => "world",
            Cell::Ru => "ru",
        }
    }

    /// The site's host, as the window names it.
    pub fn host(self) -> &'static str {
        match self {
            Cell::World => "retv.app",
            Cell::Ru => "ru.retv.app",
        }
    }

    /// The site's origin.
    pub fn site(self) -> &'static str {
        match self {
            Cell::World => "https://retv.app",
            Cell::Ru => "https://ru.retv.app",
        }
    }

    /// The API base: `<site>/v1`.
    pub fn api_base(self) -> String {
        format!("{}/v1", self.site())
    }

    pub fn other(self) -> Cell {
        match self {
            Cell::World => Cell::Ru,
            Cell::Ru => Cell::World,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_language_in_russia_or_nowhere_goes_to_ru() {
        assert_eq!(Cell::pick("ru", "Europe/Moscow"), Cell::Ru);
        assert_eq!(Cell::pick("ru-RU", "Asia/Novosibirsk"), Cell::Ru);
        assert_eq!(Cell::pick("ru", "UTC"), Cell::Ru);
        assert_eq!(Cell::pick("ru", "Etc/GMT-3"), Cell::Ru);
        assert_eq!(Cell::pick("ru", ""), Cell::Ru);
    }

    #[test]
    fn everyone_else_goes_to_the_world_site() {
        assert_eq!(Cell::pick("ru", "Europe/Berlin"), Cell::World);
        assert_eq!(Cell::pick("ru", "Europe/Riga"), Cell::World);
        assert_eq!(Cell::pick("ru", "Europe/Kyiv"), Cell::World);
        assert_eq!(Cell::pick("uk", "Europe/Moscow"), Cell::World);
        assert_eq!(Cell::pick("en", "Europe/Moscow"), Cell::World);
        assert_eq!(Cell::pick("en", "UTC"), Cell::World);
    }

    #[test]
    fn hosts_and_bases() {
        assert_eq!(Cell::Ru.api_base(), "https://ru.retv.app/v1");
        assert_eq!(Cell::World.host(), "retv.app");
        assert_eq!(Cell::World.other(), Cell::Ru);
        assert_eq!(Cell::parse("ru"), Some(Cell::Ru));
        assert_eq!(Cell::parse("RU"), None);
    }
}
