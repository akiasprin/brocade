//! Search's country footer is independent of the requested UI language. A successful
//! homepage no longer needs a country-domain redirect, so HEAD/Location is insufficient.
//! English labels use Unicode CLDR 48 region names and common Google/ISO display aliases;
//! the adjacent table contains ISO 3166-1 alpha-2 codes plus Kosovo (XK).
use crate::unlock_probe::html_attribute;

pub(super) const TARGET: &str = "https://www.google.com/?hl=en";
pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub(super) fn response_country(status: u16, body: &str) -> Option<String> {
    if !(200..300).contains(&status) {
        return None;
    }
    let mut html = body;
    let mut found = None;
    while let Some(start) = html.find('<') {
        html = &html[start + 1..];
        if let Some(comment) = html.strip_prefix("!--") {
            html = comment.split_once("-->")?.1;
            continue;
        }
        let (tag, rest) = html.split_once('>')?;
        html = rest;
        let boundary = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let (name, attributes) = tag.split_at(boundary);
        if name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style") {
            let closing = format!("</{}", name.to_ascii_lowercase());
            let end = html.to_ascii_lowercase().find(&closing)?;
            html = &html[end..];
            continue;
        }
        if !name.eq_ignore_ascii_case("div")
            || !html_attribute(attributes, "class").is_some_and(|classes| {
                classes
                    .split_ascii_whitespace()
                    .any(|class| class == "O3yKUb")
            })
        {
            continue;
        }
        let label = html.split_once('<')?.0.trim();
        if label.is_empty() || label.len() > 160 {
            return None;
        }
        let label = label
            .replace("&amp;", "&")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&nbsp;", " ");
        let country = include_str!("google-countries.tsv")
            .lines()
            .find_map(|line| {
                let (code, names) = line.split_once('\t')?;
                names
                    .split('|')
                    .any(|name| name.eq_ignore_ascii_case(&label))
                    .then(|| code.to_owned())
            })?;
        if found.as_ref().is_some_and(|previous| previous != &country) {
            return None;
        }
        found = Some(country);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_the_explicit_country_without_a_redirect_or_requested_country() {
        for (label, expected) in [
            ("Japan", "JP"),
            ("United States", "US"),
            ("United Kingdom", "GB"),
            ("Hong Kong", "HK"),
            ("Macao", "MO"),
            ("South Korea", "KR"),
            ("Türkiye", "TR"),
            ("Côte d’Ivoire", "CI"),
            ("Trinidad &amp; Tobago", "TT"),
        ] {
            let page = format!(
                "<html><div>Google</div><div data-x='1' class='footer O3yKUb'>{label}</div></html>"
            );
            assert_eq!(response_country(200, &page).as_deref(), Some(expected));
        }
        assert_eq!(TARGET, "https://www.google.com/?hl=en");
    }

    #[test]
    fn cannot_infer_country_from_unrelated_text_language_errors_or_missing_footer() {
        for page in [
            "<html lang='ja'><div>Japan</div></html>",
            "<script>const x = '<div class=\"O3yKUb\">Japan</div>';</script>",
            "<!-- <div class='O3yKUb'>Japan</div> -->",
            "<div class='O3yKUb'>Not a country</div>",
            "<div class='not-O3yKUb'>Japan</div>",
            "<div class='O3yKUb'><a>Japan</a></div>",
            "<div class='O3yKUb'>Japan</div><div class='O3yKUb'>United States</div>",
            "<html>200 OK</html>",
        ] {
            assert_eq!(response_country(200, page), None, "{page}");
        }
        assert_eq!(
            response_country(403, "<div class='O3yKUb'>Japan</div>"),
            None
        );
        assert_eq!(
            response_country(302, "<div class='O3yKUb'>Japan</div>"),
            None
        );
    }

    #[test]
    fn country_labels_have_unique_valid_codes_and_names() {
        let mut codes = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        for line in include_str!("google-countries.tsv").lines() {
            let (code, names) = line.split_once('\t').unwrap();
            assert!(code.len() == 2 && code.bytes().all(|c| c.is_ascii_uppercase()));
            assert!(codes.insert(code));
            for name in names.split('|') {
                assert!(!name.is_empty() && labels.insert(name.to_lowercase()));
            }
        }
        assert_eq!(codes.len(), 250);
    }
}
