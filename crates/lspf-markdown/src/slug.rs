//! GitHub-style heading slugs, ported from the upstream `githubSlugifier`.

use std::collections::HashMap;

/// Slug one heading's rendered text the way GitHub builds anchors: trim,
/// lowercase, drop punctuation and symbols, and turn each whitespace
/// character into `-`.
pub(crate) fn slugify(heading: &str) -> String {
    heading
        .trim()
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|character| {
            if character.is_whitespace() {
                Some('-')
            } else if character.is_alphanumeric() || matches!(character, '-' | '_') {
                Some(character)
            } else {
                None
            }
        })
        .collect()
}

/// Compare a link fragment with a heading slug. GitHub treats anchors
/// case-insensitively, and editors often percent-encode non-ASCII fragments.
pub(crate) fn fragment_matches(fragment: &str, slug: &str) -> bool {
    let decoded = crate::target::percent_decode(fragment);
    decoded.to_lowercase() == slug.to_lowercase()
}

/// Assigns document-unique slugs, suffixing repeats with `-1`, `-2`, ….
#[derive(Default)]
pub(crate) struct SlugBuilder {
    seen: HashMap<String, usize>,
}

impl SlugBuilder {
    pub(crate) fn add(&mut self, heading: &str) -> String {
        let slug = slugify(heading);
        match self.seen.get_mut(&slug) {
            Some(count) => {
                *count += 1;
                slugify(&format!("{slug}-{count}"))
            }
            None => {
                self.seen.insert(slug.clone(), 0);
                slug
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_github_rules() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("  Trim me  "), "trim-me");
        assert_eq!(slugify("What's new? (v2.0)"), "whats-new-v20");
        assert_eq!(slugify("a  b"), "a--b");
        assert_eq!(slugify("snake_case-and-kebab"), "snake_case-and-kebab");
        assert_eq!(slugify("中文 标题！"), "中文-标题");
        assert_eq!(slugify("Ünïcödé"), "ünïcödé");
    }

    #[test]
    fn repeated_headings_receive_numbered_suffixes() {
        let mut builder = SlugBuilder::default();
        assert_eq!(builder.add("Intro"), "intro");
        assert_eq!(builder.add("Intro"), "intro-1");
        assert_eq!(builder.add("intro"), "intro-2");
        assert_eq!(builder.add("Other"), "other");
    }

    #[test]
    fn fragments_match_case_insensitively_and_percent_decoded() {
        assert!(fragment_matches("Install", "install"));
        assert!(fragment_matches("%E4%B8%AD%E6%96%87", "中文"));
        assert!(!fragment_matches("setup", "install"));
    }
}
