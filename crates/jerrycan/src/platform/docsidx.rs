//! Embedded AI-native docs (docs/ai) + search. The same bytes the doc-tests run.

use serde::Serialize;

/// (topic, markdown) — embedded at compile time from the SAME files the
/// doc-tests execute, so served docs can never drift from verified docs.
pub const PAGES: &[(&str, &str)] = &[
    (
        "designing",
        include_str!("../../embedded/ai/00-designing.md"),
    ),
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
    (
        "auth-advanced",
        include_str!("../../embedded/ai/16-auth-advanced.md"),
    ),
    (
        "response-types",
        include_str!("../../embedded/ai/17-response-types.md"),
    ),
    ("storage", include_str!("../../embedded/ai/18-storage.md")),
    ("realtime", include_str!("../../embedded/ai/18-realtime.md")),
    (
        "migrate-supabase",
        include_str!("../../embedded/ai/19-migrate-supabase.md"),
    ),
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

/// One row of the page index: slug + the `# Title` + a one-line summary (the
/// first prose line under `## Purpose`). Lets an agent enumerate the whole docs
/// surface in a single call instead of guessing search terms.
#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub page: String,
    pub title: String,
    pub summary: String,
}

/// The `# Title` of a page (first `# ` heading), falling back to the slug.
fn page_title(md: &str, slug: &str) -> String {
    md.lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
        .unwrap_or_else(|| slug.to_string())
}

/// A one-line summary: the first prose line under `## Purpose`, or — for the few
/// pages with no Purpose section (packaging, error-codes) — the first prose line
/// after the `# Title`. Skips blank lines, headings, and code blocks so the row
/// always reads as a sentence.
fn page_summary(md: &str) -> String {
    let mut in_purpose = false;
    let mut after_title = false;
    let mut in_code = false;
    let mut fallback: Option<String> = None;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            in_purpose = rest.trim().eq_ignore_ascii_case("purpose");
            continue;
        }
        if trimmed.starts_with('#') {
            // A `# Title` opens the body; deeper headings just aren't Purpose.
            after_title = true;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if in_purpose {
            return trimmed.to_string();
        }
        // First prose line of the page, kept as a fallback for Purpose-less pages.
        if after_title && fallback.is_none() {
            fallback = Some(trimmed.to_string());
        }
    }
    fallback.unwrap_or_default()
}

/// Every page as (slug, title, one-line summary), in `PAGES` order. The complete
/// docs surface in one call — no result cap, so nothing is hidden.
pub fn list() -> Vec<PageInfo> {
    PAGES
        .iter()
        .map(|(slug, md)| PageInfo {
            page: (*slug).to_string(),
            title: page_title(md, slug),
            summary: page_summary(md),
        })
        .collect()
}

/// Case-insensitive substring search; hits ranked by per-page match count, then
/// truncated to `limit`. A page matches at most once, so `limit >= PAGES.len()`
/// can never hide a hit. To enumerate the whole surface, use [`list`] instead of
/// a broad search with a small limit.
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
    fn realtime_page_is_served() {
        assert!(
            PAGES
                .iter()
                .any(|(t, body)| *t == "realtime" && body.contains("changes:"))
        );
    }

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

    #[test]
    fn list_enumerates_every_page_with_title_and_summary() {
        let pages = list();
        // The index covers the whole surface, 1:1 with PAGES.
        assert_eq!(pages.len(), PAGES.len());
        let slugs: Vec<&str> = pages.iter().map(|p| p.page.as_str()).collect();
        let expected: Vec<&str> = PAGES.iter().map(|(s, _)| *s).collect();
        assert_eq!(
            slugs, expected,
            "list() preserves PAGES order and covers all"
        );
        // Each row carries a real title (a `# ` heading) and a Purpose summary —
        // the two fields an agent reads to pick a page without a search.
        for p in &pages {
            assert!(!p.title.is_empty(), "page {} has a title", p.page);
            assert!(!p.summary.is_empty(), "page {} has a summary", p.page);
        }
        let testing = pages.iter().find(|p| p.page == "testing").unwrap();
        assert_eq!(testing.title, "Testing");
    }

    #[test]
    fn a_broad_search_at_the_page_count_limit_hides_nothing() {
        // "jerrycan" appears on nearly every page; with a limit at least the page
        // count, no matching page is silently truncated away (the Gap A footgun:
        // a hardcoded small cap hid pages from an enumerating agent).
        let n = PAGES.len();
        let broad = search("the", n);
        // Every hit is a distinct page (search scores each page once), so a limit
        // of `n` can never drop a page that matched.
        let mut seen: Vec<&str> = broad.iter().map(|h| h.page.as_str()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), broad.len(), "one hit per page");
        // A small cap DOES truncate — proving the cap is the thing `list()`/a
        // higher limit fixes, not an accident of the corpus.
        assert!(
            search("the", 2).len() <= 2,
            "a small limit truncates; raise it (or use list) to see all"
        );
    }
}
