//! Embedded AI-native docs (docs/ai) + search. The same bytes the doc-tests run.

use serde::Serialize;

/// (topic, markdown) — embedded at compile time from the SAME files the
/// doc-tests execute, so served docs can never drift from verified docs.
pub const PAGES: &[(&str, &str)] = &[
    ("app", include_str!("../../embedded/ai/01-app.md")),
    ("modules", include_str!("../../embedded/ai/02-modules.md")),
    (
        "extractors",
        include_str!("../../embedded/ai/03-extractors.md"),
    ),
    (
        "dependencies",
        include_str!("../../embedded/ai/04-dependencies.md"),
    ),
    ("errors", include_str!("../../embedded/ai/05-errors.md")),
    (
        "middleware",
        include_str!("../../embedded/ai/06-middleware.md"),
    ),
    ("testing", include_str!("../../embedded/ai/07-testing.md")),
    ("database", include_str!("../../embedded/ai/08-database.md")),
    (
        "validation",
        include_str!("../../embedded/ai/09-validation.md"),
    ),
    ("auth", include_str!("../../embedded/ai/10-auth.md")),
    (
        "observability",
        include_str!("../../embedded/ai/11-observability.md"),
    ),
    (
        "packaging",
        include_str!("../../embedded/ai/12-packaging.md"),
    ),
    (
        "error-codes",
        include_str!("../../embedded/ai/13-error-codes.md"),
    ),
    ("tenancy", include_str!("../../embedded/ai/14-tenancy.md")),
    ("jobs", include_str!("../../embedded/ai/15-jobs.md")),
];

fn slug(heading: &str) -> String {
    heading
        .trim_start_matches('#')
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            'a'..='z' | '0'..='9' => Some(c),
            ' ' => Some('-'),
            _ => None,
        })
        .collect()
}

/// A whole page, or one `##` section by slug ("errors-youll-hit").
pub fn get(page: &str, anchor: Option<&str>) -> Option<String> {
    let (_, md) = PAGES.iter().find(|(name, _)| *name == page)?;
    let Some(anchor) = anchor else {
        return Some((*md).to_string());
    };
    let mut collecting = false;
    let mut out = String::new();
    for line in md.lines() {
        if line.starts_with("## ") {
            if collecting {
                break;
            }
            collecting = slug(line) == anchor;
        }
        if collecting {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub page: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    pub snippet: String,
}

/// Case-insensitive substring search; hits ranked by per-page match count.
pub fn search(query: &str, limit: usize) -> Vec<SearchHit> {
    let q = query.to_lowercase();
    let mut scored: Vec<(usize, SearchHit)> = Vec::new();
    for (name, md) in PAGES {
        let mut count = 0;
        let mut first: Option<(Option<String>, String)> = None;
        let mut current_anchor: Option<String> = None;
        for line in md.lines() {
            if line.starts_with("## ") {
                current_anchor = Some(slug(line));
            }
            if line.to_lowercase().contains(&q) {
                count += 1;
                if first.is_none() {
                    first = Some((current_anchor.clone(), line.trim().to_string()));
                }
            }
        }
        if let Some((anchor, snippet)) = first {
            scored.push((
                count,
                SearchHit {
                    page: (*name).to_string(),
                    anchor,
                    snippet,
                },
            ));
        }
    }
    scored.sort_by_key(|h| std::cmp::Reverse(h.0));
    scored.into_iter().take(limit).map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_whole_pages_and_anchored_sections() {
        let page = get("dependencies", None).unwrap();
        assert!(page.contains("# Dependencies"));
        let section = get("dependencies", Some("errors-youll-hit")).unwrap();
        assert!(section.contains("JC1001"));
        assert!(
            !section.contains("## Minimal example"),
            "section slice only"
        );
        assert!(get("nonsense", None).is_none());
    }

    #[test]
    fn search_finds_pages_with_anchors_and_snippets() {
        let results = search("override_dep", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].page, "testing");
        assert!(results[0].snippet.to_lowercase().contains("override"));
        assert!(search("zzz-not-a-real-term", 5).is_empty());
    }
}
