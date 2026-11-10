//! The terminal's font family: a comma-separated list in fallback order, the
//! way VS Code's `editor.fontFamily` works.
//!
//! gpui 0.2.2 ships no font files, and its built-in fallback stack is
//! proportional-only: asking it for the CSS generic `monospace` silently
//! resolves to a proportional system font on Windows, so the grid's columns go
//! ragged. This module turns the saved list into the family names gpui can
//! actually look up, in order, so the terminal ends up monospace.

/// The built-in fallback order, from Orbit `src/lib/fontFamily.ts`
/// (`TERMINAL_FONT_STACK`).
///
/// "Symbols Nerd Font Mono" comes first: it draws the Nerd Font icons a prompt
/// prints, which no system font has. It never sizes a cell, so the text fonts
/// after it are what the grid measures.
///
/// The CSS generic `monospace` sits in this stack exactly as the reference
/// writes it, because that string is also the CSS `font-family` the web client
/// sends. GPUI cannot use it (see [`terminal_font_families`]), so the family
/// list drops it before the lookup.
pub const TERMINAL_FONT_STACK: &str = "\"Symbols Nerd Font Mono\", \"JetBrains Mono\", \"Fira Code\", \"D2Coding\", Menlo, Monaco, \"Cascadia Code\", \"Cascadia Mono\", Consolas, \"Noto Sans Mono CJK KR\", monospace, \"Malgun Gothic\"";

/// How many characters a sanitized family list may hold, from the reference's
/// `FONT_FAMILY_MAX_CHARS`.
pub const FONT_FAMILY_MAX_CHARS: usize = 200;

/// A typed family list as a CSS `font-family` value: unsafe characters
/// dropped, each name trimmed, empty ones removed, names CSS cannot read bare
/// quoted. Names past the length limit are dropped whole, so a cut never
/// leaves an open quote.
///
/// Port of Orbit `sanitizeFontFamily`. Two reference steps have no plain
/// equivalent here: its NFC composition (std has no Unicode normalization,
/// and this crate takes no new dependencies) is left out, and its 200-char
/// limit counts UTF-16 code units in the browser while this counts `char`s.
pub fn sanitize_font_family(value: &str) -> String {
    let cleaned: String = value.chars().filter(|c| !is_unsafe(*c)).collect();
    let mut family = String::new();
    for name in cleaned.split(',') {
        let name = normalize_name(name);
        if name.is_empty() {
            continue;
        }
        let next = if family.is_empty() {
            name
        } else {
            format!("{family}, {name}")
        };
        if next.chars().count() > FONT_FAMILY_MAX_CHARS {
            // The name goes whole: a cut must never leave an open quote.
            break;
        }
        family = next;
    }
    family
}

/// What could close the declaration, escape, or hide in the value.
fn is_unsafe(character: char) -> bool {
    matches!(
        character,
        ';' | '{' | '}' | '<' | '>' | '\\' | '\u{0}'..='\u{1f}' | '\u{7f}'
    )
}

/// A name CSS reads bare: one identifier, non-ASCII letters included, no spaces.
fn is_identifier(name: &str) -> bool {
    let name = name.strip_prefix('-').unwrap_or(name);
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || !first.is_ascii()) {
        return false;
    }
    characters.all(|character| {
        character.is_alphanumeric() || character == '_' || character == '-' || !character.is_ascii()
    })
}

/// Unquoted, these are CSS keywords rather than names, and make the whole list invalid.
fn is_reserved(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
    )
}

/// One name from the list: unquoted, its inner quotes gone, its whitespace runs
/// collapsed to one space.
fn normalize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let quoted = trimmed.chars().count() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')));
    let inner = if quoted {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    let name: String = inner
        .chars()
        .filter(|character| *character != '"' && *character != '\'')
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return String::new();
    }
    // A name the user quoted stays quoted: "monospace" is a font called
    // monospace, not the generic.
    if quoted || !is_identifier(&name) || is_reserved(&name) {
        format!("\"{name}\"")
    } else {
        name
    }
}

/// A stack split into bare family names: unquoted, trimmed, empties dropped.
///
/// This is what a font lookup takes, so it always sees `Cascadia Code` rather
/// than `"Cascadia Code"`.
pub fn family_names(stack: &str) -> Vec<String> {
    stack
        .split(',')
        .map(|name| {
            name.trim()
                .trim_matches(|character| character == '"' || character == '\'')
                .trim()
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The family list to try, in order: the user's sanitized list first, then the
/// built-in stack. Names are bare, as gpui's font lookup takes them.
///
/// The CSS generic `monospace` is not offered: gpui's font lookup has no
/// generic family, and on Windows it quietly resolves `monospace` to a
/// *proportional* system font, which is the ragged-columns bug this model
/// fixes. A user list that literally names a font "monospace" is
/// indistinguishable from the generic after unquoting, so it is dropped too.
pub fn terminal_font_families(chosen: &str) -> Vec<String> {
    let mut families: Vec<String> = Vec::new();
    for name in family_names(&sanitize_font_family(chosen))
        .into_iter()
        .chain(family_names(TERMINAL_FONT_STACK))
    {
        if name.eq_ignore_ascii_case("monospace") {
            continue;
        }
        if families
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        families.push(name);
    }
    families
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_a_space_is_quoted() {
        assert_eq!(sanitize_font_family("JetBrains Mono"), "\"JetBrains Mono\"");
    }

    #[test]
    fn a_bare_identifier_stays_unquoted() {
        assert_eq!(sanitize_font_family("Consolas"), "Consolas");
    }

    #[test]
    fn a_reserved_css_keyword_is_quoted() {
        assert_eq!(sanitize_font_family("inherit"), "\"inherit\"");
        assert_eq!(sanitize_font_family("Revert-Layer"), "\"Revert-Layer\"");
    }

    #[test]
    fn an_unsafe_character_is_dropped() {
        assert_eq!(sanitize_font_family("Foo;Bar"), "FooBar");
        assert_eq!(sanitize_font_family("Foo{Bar}<Baz>\\Qux"), "FooBarBazQux");
    }

    #[test]
    fn a_control_character_is_dropped() {
        assert_eq!(
            sanitize_font_family("Foo\u{0}Bar\u{1f}Baz\u{7f}"),
            "FooBarBaz"
        );
    }

    #[test]
    fn a_stray_quote_inside_a_name_is_removed() {
        // A bare name keeps the quotes out of the CSS string it becomes.
        assert_eq!(sanitize_font_family("Foo\"Bar"), "FooBar");
        // A quoted name stays quoted, with the inner quote gone.
        assert_eq!(sanitize_font_family("\"Foo\"Bar\""), "\"FooBar\"");
    }

    #[test]
    fn a_quoted_name_is_kept_quoted_even_when_it_could_be_bare() {
        assert_eq!(sanitize_font_family("\"Consolas\""), "\"Consolas\"");
    }

    #[test]
    fn internal_whitespace_runs_collapse_to_one_space() {
        assert_eq!(
            sanitize_font_family("  Cascadia   Code  "),
            "\"Cascadia Code\""
        );
    }

    #[test]
    fn the_length_cap_drops_a_whole_trailing_name() {
        let long = "A".repeat(190);
        let value = format!("{long}, {}", "B".repeat(20));
        let family = sanitize_font_family(&value);
        // 190 + 2 + 20 passes the cap, so the trailing name goes whole: the
        // result is the first name, not a half-cut one and not an open quote.
        assert_eq!(family, long);
        assert!(family.chars().count() <= FONT_FAMILY_MAX_CHARS);
    }

    #[test]
    fn an_empty_value_yields_an_empty_list() {
        assert_eq!(sanitize_font_family(""), "");
        assert!(family_names("").is_empty());
    }

    #[test]
    fn a_stack_splits_into_bare_names() {
        assert_eq!(
            family_names("\"Cascadia Code\", Consolas , Menlo"),
            vec![
                "Cascadia Code".to_string(),
                "Consolas".to_string(),
                "Menlo".to_string()
            ]
        );
    }

    #[test]
    fn an_empty_choice_offers_the_built_in_families_in_order() {
        let families = terminal_font_families("");
        assert_eq!(
            families,
            vec![
                "Symbols Nerd Font Mono".to_string(),
                "JetBrains Mono".to_string(),
                "Fira Code".to_string(),
                "D2Coding".to_string(),
                "Menlo".to_string(),
                "Monaco".to_string(),
                "Cascadia Code".to_string(),
                "Cascadia Mono".to_string(),
                "Consolas".to_string(),
                "Noto Sans Mono CJK KR".to_string(),
                "Malgun Gothic".to_string(),
            ]
        );
    }

    #[test]
    fn a_chosen_family_comes_before_the_built_in_stack() {
        let families = terminal_font_families("Iosevka");
        assert_eq!(families.first().map(String::as_str), Some("Iosevka"));
        assert_eq!(
            families.get(1).map(String::as_str),
            Some("Symbols Nerd Font Mono")
        );
        assert_eq!(families.last().map(String::as_str), Some("Malgun Gothic"));
    }

    #[test]
    fn a_chosen_name_is_bare_for_the_font_lookup() {
        let families = terminal_font_families("JetBrains Mono");
        assert_eq!(families.first().map(String::as_str), Some("JetBrains Mono"));
    }

    #[test]
    fn the_css_generic_monospace_is_not_offered_to_the_font_lookup() {
        let families = terminal_font_families("");
        assert!(!families
            .iter()
            .any(|name| name.eq_ignore_ascii_case("monospace")));
        // Even typed explicitly, the generic is not a family gpui can use.
        let typed = terminal_font_families("monospace");
        assert!(!typed
            .iter()
            .any(|name| name.eq_ignore_ascii_case("monospace")));
    }
}
