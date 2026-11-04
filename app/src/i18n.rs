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

    /// The first of the system's preferred languages that Bridge speaks
    /// (ru, uk, en), in the person's order; English when none is.
    /// `["de-DE", "ru-RU"]` → Ru: a German-first Mac with Russian second
    /// reads Russian, not English.
    pub fn from_locales<I, S>(locales: I) -> Lang
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for l in locales {
            let l = l.as_ref().to_ascii_lowercase();
            if l.starts_with("ru") || l.starts_with("uk") || l.starts_with("en") {
                return Lang::from_locale(&l);
            }
        }
        Lang::En
    }

    /// The system's language. Debug builds honour `RETV_BRIDGE_LANG` (ru, en, uk)
    /// so every language can be checked on one machine.
    pub fn detect() -> Lang {
        if cfg!(debug_assertions) {
            if let Ok(v) = std::env::var("RETV_BRIDGE_LANG") {
                return Lang::from_locale(&v);
            }
        }
        Lang::from_locales(sys_locale::get_locales())
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
            Lang::Uk => "uk",
        }
    }
}

/// The language menu: «Как в системе», then each language by its own name.
pub const LANG_CHOICES: [&str; 4] = ["system", "ru", "uk", "en"];

pub fn valid_choice(code: &str) -> bool {
    LANG_CHOICES.contains(&code)
}

/// The language the person picked in Bridge (`ru`, `uk`, `en`), else the system's.
pub fn pick(saved: Option<&str>, system: Lang) -> Lang {
    match saved {
        Some("ru") => Lang::Ru,
        Some("uk") => Lang::Uk,
        Some("en") => Lang::En,
        _ => system,
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
    fn lang_from_the_preferred_list() {
        assert_eq!(Lang::from_locales(["ru-RU", "en-US"]), Lang::Ru);
        assert_eq!(Lang::from_locales(["de-DE", "ru-RU"]), Lang::Ru);
        assert_eq!(Lang::from_locales(["pl-PL", "uk-UA", "ru-RU"]), Lang::Uk);
        assert_eq!(Lang::from_locales(["en-RU", "ru-RU"]), Lang::En);
        assert_eq!(Lang::from_locales(["de-DE"]), Lang::En);
        assert_eq!(Lang::from_locales(Vec::<String>::new()), Lang::En);
    }

    #[test]
    fn a_language_the_person_picked_wins_over_the_system() {
        // The owner's Mac: preferred languages [en, ru], so the system pick is English.
        let system = Lang::from_locales(["en", "ru"]);
        assert_eq!(system, Lang::En);
        assert_eq!(pick(Some("ru"), system), Lang::Ru);
        assert_eq!(pick(Some("uk"), system), Lang::Uk);
        assert_eq!(pick(Some("system"), system), Lang::En);
        assert_eq!(pick(None, Lang::Ru), Lang::Ru);
        assert_eq!(
            pick(Some("de"), Lang::Ru),
            Lang::Ru,
            "an unknown code is the system"
        );
        for c in LANG_CHOICES {
            assert!(valid_choice(c));
            assert_ne!(t(Lang::En, &format!("lang.{c}")), format!("lang.{c}"));
        }
        assert!(!valid_choice("de"));
    }

    #[test]
    fn plural_strings_have_every_form() {
        // The window picks `.one/.few/.many/.other` with Intl.PluralRules (ru and uk use all four).
        for lang in [Lang::Ru, Lang::En, Lang::Uk] {
            let table = strings(lang);
            for k in table.keys().filter(|k| k.ends_with(".one")) {
                let base = &k[..k.len() - ".one".len()];
                for form in ["few", "many", "other"] {
                    let key = format!("{base}.{form}");
                    assert!(table.contains_key(&key), "{lang:?} misses {key}");
                }
            }
        }
        assert_eq!(t(Lang::Ru, "samsung.apps.uses.few"), "{n} раза");
        assert_eq!(t(Lang::Uk, "samsung.apps.uses.many"), "{n} разів");
    }

    #[test]
    fn world_strings_never_name_russia() {
        for lang in [Lang::En, Lang::Uk] {
            for (k, v) in strings(lang) {
                let low = v.to_lowercase();
                for word in ["russia", "росі", "россі"] {
                    assert!(!low.contains(word), "{lang:?}: {k} mentions Russia: {v}");
                }
            }
        }
    }

    #[test]
    fn no_window_string_has_a_check_mark() {
        // Done states get an icon in the window; strings stay plain.
        for lang in [Lang::Ru, Lang::En, Lang::Uk] {
            for (k, v) in strings(lang) {
                assert!(!v.contains('✓'), "{lang:?}: {k} has a check mark");
            }
        }
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
    fn samsung_strings_do_not_name_the_mac() {
        // The Samsung tab ships on Windows too.
        for lang in [Lang::Ru, Lang::En, Lang::Uk] {
            for (k, v) in strings(lang) {
                if k.starts_with("samsung.") {
                    assert!(!v.contains("Mac"), "{lang:?}: {k} names the Mac: {v}");
                }
            }
        }
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
