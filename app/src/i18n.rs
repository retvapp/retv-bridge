//! The app's strings in Russian, English and Ukrainian (`locales/*.json`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ru,
    En,
    Uk,
}

impl Lang {
    /// `ru*` → Ru, `uk*` → Uk, anything else → En.
    pub fn from_locale(locale: &str) -> Lang {
        let l = locale.to_ascii_lowercase();
        if l.starts_with("ru") {
            Lang::Ru
        } else if l.starts_with("uk") {
            Lang::Uk
        } else {
            Lang::En
        }
    }

    /// The system's language. Debug builds honour `RETV_BRIDGE_LANG` (ru, en, uk)
    /// so every language can be checked on one machine.
    pub fn detect() -> Lang {
        if cfg!(debug_assertions) {
            if let Ok(v) = std::env::var("RETV_BRIDGE_LANG") {
                return Lang::from_locale(&v);
            }
        }
        Lang::from_locale(&sys_locale::get_locale().unwrap_or_default())
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
            Lang::Uk => "uk",
        }
    }
}

const RU: &str = include_str!("../locales/ru.json");
const EN: &str = include_str!("../locales/en.json");
const UK: &str = include_str!("../locales/uk.json");

pub type Strings = BTreeMap<String, String>;

/// Every string of `lang`.
pub fn strings(lang: Lang) -> &'static Strings {
    static TABLES: OnceLock<[Strings; 3]> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        let parse = |s: &str| serde_json::from_str::<Strings>(s).expect("bad locale JSON");
        [parse(RU), parse(EN), parse(UK)]
    });
    &tables[lang as usize]
}

/// The string `key` in `lang`; English, then the key itself, if missing.
pub fn t(lang: Lang, key: &str) -> String {
    strings(lang)
        .get(key)
        .or_else(|| strings(Lang::En).get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// `t` with `{name}` placeholders filled in.
pub fn tf(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(lang, key);
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_from_locale() {
        assert_eq!(Lang::from_locale("ru-RU"), Lang::Ru);
        assert_eq!(Lang::from_locale("ru"), Lang::Ru);
        assert_eq!(Lang::from_locale("uk-UA"), Lang::Uk);
        assert_eq!(Lang::from_locale("uk"), Lang::Uk);
        assert_eq!(Lang::from_locale("en-GB"), Lang::En);
        assert_eq!(Lang::from_locale("de-DE"), Lang::En);
        assert_eq!(Lang::from_locale(""), Lang::En);
    }

    #[test]
    fn i18n_keys_match_across_ru_en_uk() {
        let en: Vec<&String> = strings(Lang::En).keys().collect();
        assert!(en.len() > 20);
        for lang in [Lang::Ru, Lang::Uk] {
            let keys: Vec<&String> = strings(lang).keys().collect();
            assert_eq!(keys, en, "{lang:?} has a different key set");
        }
        for lang in [Lang::Ru, Lang::En, Lang::Uk] {
            for (k, v) in strings(lang) {
                assert!(!v.trim().is_empty(), "{lang:?}: {k} is empty");
            }
        }
    }

    #[test]
    fn the_brief_copy_is_verbatim() {
        assert_eq!(t(Lang::Ru, "title"), "ReTV Bridge работает");
        assert_eq!(t(Lang::Ru, "code.label"), "Код для сайта");
        assert_eq!(t(Lang::Ru, "quit.confirm"), "Идёт работа с ТВ. Выйти?");
        assert_eq!(
            tf(Lang::Ru, "tray.tv", &[("model", "MiTV-AFMU0")]),
            "ТВ: MiTV-AFMU0, подключён"
        );
        assert_eq!(
            tf(Lang::Ru, "notify.code", &[("code", "482 917")]),
            "Код для сайта: 482 917"
        );
        assert_eq!(t(Lang::Uk, "nope.missing"), "nope.missing");
    }

    #[test]
    fn no_window_string_has_a_long_dash() {
        for (name, text) in [
            ("en", include_str!("../locales/en.json")),
            ("ru", include_str!("../locales/ru.json")),
            ("uk", include_str!("../locales/uk.json")),
        ] {
            assert!(!text.contains('\u{2014}'), "{name}.json has a long dash");
        }
    }
}
