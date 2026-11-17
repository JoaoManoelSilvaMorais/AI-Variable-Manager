use std::collections::HashSet;

use unicode_normalization::UnicodeNormalization;

const KNOWN_IMAGE_EXTENSIONS: &[&str] = &[".jpeg", ".tiff", ".jpg", ".png", ".tif"];

/// Trims leading/trailing whitespace and collapses internal whitespace runs into single spaces.
pub fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_combining_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{1AB0}'..='\u{1AFF}').contains(&c)
        || ('\u{1DC0}'..='\u{1DFF}').contains(&c)
        || ('\u{20D0}'..='\u{20FF}').contains(&c)
        || ('\u{FE20}'..='\u{FE2F}').contains(&c)
}

/// Decomposes via NFD, strips combining diacritical marks, and recomposes via NFC.
pub fn strip_diacritics_nfd_nfc(input: &str) -> String {
    let stripped: String = input.nfd().filter(|&c| !is_combining_mark(c)).collect();
    stripped.nfc().collect()
}

/// Normalizes a file stem (without extension) without mutating the source string:
/// trims & collapses whitespace, strips diacritics (NFD->NFC), and converts to lowercase.
pub fn normalize_stem(stem: &str) -> String {
    let collapsed = collapse_whitespace(stem);
    strip_diacritics_nfd_nfc(&collapsed).to_lowercase()
}

/// Normalizes a record match key without mutating the source string:
/// trims & collapses whitespace, strips a trailing known image extension
/// (`.jpg`, `.jpeg`, `.png`, `.tiff`, `.tif`) case-insensitively if preceded by
/// at least 1 character, strips diacritics (NFD->NFC), and converts to lowercase.
pub fn normalize_match_key(input: &str) -> String {
    let collapsed = collapse_whitespace(input);
    let lower = collapsed.to_ascii_lowercase();
    let without_ext = KNOWN_IMAGE_EXTENSIONS
        .iter()
        .find(|&&ext| lower.ends_with(ext) && collapsed.len() > ext.len())
        .map(|&ext| &collapsed[..collapsed.len() - ext.len()])
        .unwrap_or(collapsed.as_str());
    normalize_stem(without_ext)
}

fn is_ncname_start_char(c: char) -> bool {
    matches!(
        c,
        'A'..='Z'
            | '_'
            | 'a'..='z'
            | '\u{C0}'..='\u{D6}'
            | '\u{D8}'..='\u{F6}'
            | '\u{F8}'..='\u{2FF}'
            | '\u{370}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
            | '\u{10000}'..='\u{EFFFF}'
    )
}

fn is_ncname_char(c: char) -> bool {
    is_ncname_start_char(c)
        || matches!(
            c,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'
        )
}

/// Validates an XML 1.0 (5th Ed.) NCName (NameStartChar + NameChar* excluding `:`).
pub fn is_valid_xml_ncname(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_ncname_start_char(first) && chars.all(is_ncname_char)
}

/// Deterministically sanitizes an arbitrary display name into an ASCII-safe XML NCName
/// accepted by Adobe Illustrator and `quick-xml`.
pub fn suggest_xml_name(display_name: &str) -> String {
    let ascii_base = strip_diacritics_nfd_nfc(display_name.trim());
    let mut mapped = String::with_capacity(ascii_base.len());
    let mut prev_underscore = false;

    for c in ascii_base.chars() {
        if c.is_ascii_alphanumeric() {
            mapped.push(c);
            prev_underscore = false;
        } else if !prev_underscore {
            mapped.push('_');
            prev_underscore = true;
        }
    }

    let trimmed = mapped.trim_matches('_');
    if trimmed.is_empty() {
        return "var_1".to_string();
    }

    if trimmed.as_bytes()[0].is_ascii_digit() {
        format!("var_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Resolves a unique valid XML NCName against an existing set of names.
pub fn resolve_unique_xml_name<'a, I>(candidate: &str, existing: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let used: HashSet<&str> = existing.into_iter().collect();
    let trimmed = candidate.trim();
    let base = if is_valid_xml_ncname(trimmed) {
        trimmed.to_string()
    } else {
        suggest_xml_name(trimmed)
    };

    if !used.contains(base.as_str()) {
        return base;
    }

    let mut suffix = 2usize;
    loop {
        let attempt = format!("{base}_{suffix}");
        if !used.contains(attempt.as_str()) {
            return attempt;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_match_key_preserves_source_and_handles_diacritics_whitespace_and_extensions() {
        let raw = "  João\u{00A0}  Manoel\tConceição.JPG ";
        assert_eq!(normalize_match_key(raw), "joao manoel conceicao");

        let nfc = "José Ávila";
        let nfd = "Jose\u{0301} A\u{0301}vila";
        assert_eq!(normalize_match_key(nfc), normalize_match_key(nfd));
        assert_eq!(normalize_match_key(nfc), "jose avila");

        assert_eq!(
            normalize_stem("ÁÉÍÓÚÀÂÊÔÃÕÜÇ áéíóúàâêôãõüç"),
            "aeiouaaeoaouc aeiouaaeoaouc"
        );
        assert_eq!(normalize_match_key("MARIA_CLARA"), "maria_clara");
        assert_eq!(normalize_match_key("Dr. João Silva"), "dr. joao silva");
        assert_eq!(
            normalize_match_key("BD9A5505.jpg.jpeg"),
            "bd9a5505.jpg"
        );
        assert_eq!(normalize_match_key(".jpg"), ".jpg");
    }

    #[test]
    fn xml_ncname_validation_matches_xml_1_0_excluding_colon() {
        assert!(is_valid_xml_ncname("Nome"));
        assert!(is_valid_xml_ncname("_id"));
        assert!(is_valid_xml_ncname("Nome_1"));
        assert!(is_valid_xml_ncname("a-b.c"));
        assert!(is_valid_xml_ncname("Função"));
        assert!(is_valid_xml_ncname("var_1"));

        assert!(!is_valid_xml_ncname(""));
        assert!(!is_valid_xml_ncname("123"));
        assert!(!is_valid_xml_ncname("1abc"));
        assert!(!is_valid_xml_ncname("a:b"));
        assert!(!is_valid_xml_ncname(":abc"));
        assert!(!is_valid_xml_ncname("a b"));
        assert!(!is_valid_xml_ncname("-abc"));
        assert!(!is_valid_xml_ncname(".abc"));
        assert!(!is_valid_xml_ncname("a/b"));
        assert!(!is_valid_xml_ncname("a<b"));
    }

    #[test]
    fn suggest_xml_name_produces_deterministic_ascii_ncnames() {
        assert_eq!(suggest_xml_name("Nome Completo"), "Nome_Completo");
        assert_eq!(suggest_xml_name("Função / Cargo"), "Funcao_Cargo");
        assert_eq!(suggest_xml_name("1º Código (CEP)"), "var_1_Codigo_CEP");
        assert_eq!(suggest_xml_name("1º Lugar"), "var_1_Lugar");
        assert_eq!(suggest_xml_name("___---___"), "var_1");
        assert_eq!(suggest_xml_name("   "), "var_1");
        assert_eq!(suggest_xml_name("  __Nome__  "), "Nome");
        assert_eq!(suggest_xml_name("xml:space"), "xml_space");

        for sample in [
            "",
            "   ",
            "???",
            "123",
            "Função / Cargo",
            "v:Nome",
            "São João",
        ] {
            let suggested = suggest_xml_name(sample);
            assert!(
                is_valid_xml_ncname(&suggested),
                "expected valid NCName for '{sample}', got '{suggested}'"
            );
        }
    }

    #[test]
    fn resolve_unique_xml_name_disambiguates_collisions() {
        assert_eq!(
            resolve_unique_xml_name("Nome", ["Nome", "Nome_2"]),
            "Nome_3"
        );
        assert_eq!(resolve_unique_xml_name("", ["var_1"]), "var_1_2");
        assert_eq!(resolve_unique_xml_name("Cargo", ["Nome"]), "Cargo");
    }
}
