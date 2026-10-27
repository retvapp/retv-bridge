//! Which ReTV site the bridge opens, and the link that carries the token.

use retv_bridge_core::protocol::ALLOWED_ORIGINS;

/// The ReTV site for a system locale: the Russian site for `ru*`, else the
/// international one.
pub fn site_for(locale: &str) -> &'static str {
    if locale.to_ascii_lowercase().starts_with("ru") {
        "https://ru.retv.app"
    } else {
        "https://retv.app"
    }
}

/// The link the bridge opens. The token sits in the fragment, which browsers
/// never send to the site's server.
pub fn connect_link(site: &str, token: &str) -> String {
    format!("{site}/connect?path=bridge#bridgeToken={token}") // leakcheck:allow
}

/// A development override of the site (`RETV_SITE_URL`, debug builds only):
/// accepted only when it is exactly an allowed origin, so the token never
/// goes anywhere else.
pub fn dev_override(value: Option<&str>) -> Option<&'static str> {
    let v = value?.trim_end_matches('/');
    ALLOWED_ORIGINS.iter().copied().find(|o| *o == v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_for_ru_locales() {
        assert_eq!(site_for("ru-RU"), "https://ru.retv.app");
        assert_eq!(site_for("ru"), "https://ru.retv.app");
        assert_eq!(site_for("RU_ru"), "https://ru.retv.app");
        assert_eq!(site_for("uk-UA"), "https://retv.app");
        assert_eq!(site_for("en-US"), "https://retv.app");
        assert_eq!(site_for("de"), "https://retv.app");
        assert_eq!(site_for(""), "https://retv.app");
    }

    #[test]
    fn connect_link_puts_token_in_fragment() {
        let link = connect_link("https://retv.app", "abcDEF-_123");
        assert_eq!(
            link,
            "https://retv.app/connect?path=bridge#bridgeToken=abcDEF-_123" // leakcheck:allow
        );
        let (before, after) = link.split_once('#').unwrap();
        assert!(
            !before.contains("abcDEF"),
            "the token must not be in the path or query"
        );
        assert_eq!(after, "bridgeToken=abcDEF-_123"); // leakcheck:allow
    }

    #[test]
    fn dev_override_takes_only_allowed_origins() {
        assert_eq!(
            dev_override(Some("http://localhost:5180/")),
            Some("http://localhost:5180")
        );
        assert_eq!(dev_override(Some("https://retv.app.evil.example")), None);
        assert_eq!(dev_override(Some("http://localhost:5181")), None);
        assert_eq!(dev_override(None), None);
    }
}
