//! The operating system's language, for text the backend words itself.
//!
//! Dialogs whose wording must not come from the webview use the system
//! language rather than the UI's: the webview names the UI language, so it
//! could pick one the user does not read. On Linux the system languages come
//! from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` and `LANG`, which the app
//! inherits at launch and never changes, so the webview cannot set them
//! either.

/// The entry in `table`, keyed by the codes in `SUPPORTED_LANGUAGES`
/// (src/stores/settingsStore.ts), for the first of the user's preferred
/// system languages that has one.
pub(super) fn lookup<T>(table: &'static [(&'static str, T)]) -> Option<&'static T> {
    lookup_in(table, sys_locale::get_locales())
}

/// The entry in `table` for the first of `languages` (most preferred
/// first) that has one.
pub(super) fn lookup_in<T>(
    table: &'static [(&'static str, T)],
    languages: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<&'static T> {
    languages
        .into_iter()
        .find_map(|language| for_language(table, language.as_ref()))
}

/// The entry for a language such as `"de"`, if there is one. A region
/// (`"pt-BR"`) is ignored.
fn for_language<T>(table: &'static [(&'static str, T)], language: &str) -> Option<&'static T> {
    let primary = language.split(['-', '_']).next().unwrap_or_default();
    table
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(primary))
        .map(|(_, entry)| entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TABLE: [(&str, u8); 3] = [("en", 0), ("de", 1), ("zh", 2)];

    #[test]
    fn lookup_ignores_region_and_case() {
        assert_eq!(lookup_in(&TABLE, ["de"]), Some(&1));
        assert_eq!(lookup_in(&TABLE, ["de-AT"]), Some(&1));
        assert_eq!(lookup_in(&TABLE, ["ZH_cn"]), Some(&2));
    }

    #[test]
    fn the_first_preferred_language_with_an_entry_wins() {
        assert_eq!(lookup_in(&TABLE, ["cs-CZ", "de-DE", "en-US"]), Some(&1));
        assert_eq!(lookup_in(&TABLE, ["en-US", "de-DE"]), Some(&0));
    }

    #[test]
    fn unknown_languages_have_no_entry() {
        assert_eq!(lookup_in(&TABLE, [""; 0]), None);
        for language in ["", "xx", "-", "C", "\u{202e}de"] {
            assert_eq!(lookup_in(&TABLE, [language]), None, "{language:?}");
        }
    }
}
