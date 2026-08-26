#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OnAbsentValue {
    Allow,
    Check,
}

/// Returns true if `value` matches any of `patterns`
/// Empty pattern list is treated as a permissive match-all.
pub fn permissive_match(
    patterns: &[glob::Pattern],
    value: Option<&str>,
    on_absent_value: OnAbsentValue,
) -> bool {
    patterns.is_empty()
        || match value {
            None => match on_absent_value {
                OnAbsentValue::Allow => true,
                OnAbsentValue::Check => patterns.iter().any(|p| p.matches("")),
            },
            Some(v) => patterns.iter().any(|p| p.matches(v)),
        }
}

/// Pattern specificity as a sort key. Higher = more specific.
/// Pattern with more literal (non-`*`) characters wins
/// If equal, fewer `*` wildcards wins
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct PatternSpecificityScore {
    literals: usize,
    wildcards: std::cmp::Reverse<usize>,
}

impl From<&glob::Pattern> for PatternSpecificityScore {
    fn from(pattern: &glob::Pattern) -> Self {
        let wildcards = pattern.as_str().chars().filter(|&c| c == '*').count();
        let literals = pattern.as_str().chars().count() - wildcards;
        Self {
            literals,
            wildcards: std::cmp::Reverse(wildcards),
        }
    }
}

/// Returns the most-specific item whose pattern matches `value`
/// or `None` if no pattern matches.
/// On tie, match with the first pattern
pub fn most_specific_match<'a, T>(
    items: &'a [T],
    value: &str,
    key: impl Fn(&T) -> &glob::Pattern,
) -> Option<&'a T> {
    let mut best: Option<(&'a T, PatternSpecificityScore)> = None;
    for item in items {
        let pattern = key(item);
        if !pattern.matches(value) {
            continue;
        }
        let score = PatternSpecificityScore::from(pattern);
        match best {
            Some((_, ref best_score)) if score <= *best_score => {}
            _ => best = Some((item, score)),
        }
    }
    best.map(|(item, _)| item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(s: &str) -> glob::Pattern {
        glob::Pattern::new(s).unwrap()
    }

    fn get_most_specific_pattern<'a>(
        patterns: &'a [glob::Pattern],
        value: &str,
    ) -> Option<&'a str> {
        most_specific_match(patterns, value, |p| p).map(|p| p.as_str())
    }

    #[test]
    fn absent_value_is_allowed_or_checked() {
        let patterns = vec![pattern("claude-*")];

        assert!(permissive_match(&patterns, None, OnAbsentValue::Allow));
        assert!(!permissive_match(&patterns, None, OnAbsentValue::Check));

        // A present value is unaffected by the argument.
        assert!(permissive_match(
            &patterns,
            Some("claude-opus-5"),
            OnAbsentValue::Check
        ));
        assert!(!permissive_match(
            &patterns,
            Some("gpt-4o"),
            OnAbsentValue::Allow
        ));
        // An empty pattern list stays permissive either way.
        assert!(permissive_match(&[], None, OnAbsentValue::Check));

        // `Check` matches an absent value as the empty string, so a wildcard
        // still accepts it.
        assert!(permissive_match(
            &[pattern("*")],
            None,
            OnAbsentValue::Check
        ));
    }

    #[test]
    fn wildcard_position_does_not_matter_when_matching_most_specific_pattern() {
        // For "claude-opus-4-8", literal counts: claude-* (7), *-opus-4-8 (9),
        // claude-*-4-8 (11), claude-opus-4* (13). Highest literal count wins.
        let patterns = vec![
            pattern("claude-*"),
            pattern("*-opus-4-8"),
            pattern("claude-*-4-8"),
            pattern("claude-opus-4*"),
        ];
        assert_eq!(
            get_most_specific_pattern(&patterns, "claude-opus-4-8"),
            Some("claude-opus-4*")
        );
    }

    #[test]
    fn order_does_not_matter_when_matching_most_specific_pattern() {
        let value = "claude-opus-4-8";
        let shuffled = vec![
            pattern("claude-*-4-8"),
            pattern("claude-opus-4*"),
            pattern("claude-*"),
            pattern("*-opus-4-8"),
        ];
        assert_eq!(
            get_most_specific_pattern(&shuffled, value),
            Some("claude-opus-4*")
        );
    }

    #[test]
    fn literal_count_tie_broken_by_fewer_wildcards_when_matching_most_specific_pattern() {
        // Both match "abcd" with equal literal count (3: "a", "b", "d"), but
        // "ab*d" has one wildcard vs "a*b*d" with two. Fewer wildcards wins.
        let patterns = vec![pattern("a*b*d"), pattern("ab*d")];
        assert_eq!(get_most_specific_pattern(&patterns, "abxxbd"), Some("ab*d"));
    }

    #[test]
    fn full_tie_returns_earliest_declared_when_matching_most_specific_pattern() {
        // Two patterns, equal literal count (1) and equal wildcard count (1),
        // both matching "ab". Earliest-declared wins regardless of order.
        let forward = vec![pattern("a*"), pattern("*b")];
        assert_eq!(get_most_specific_pattern(&forward, "ab"), Some("a*"));

        let swapped = vec![pattern("*b"), pattern("a*")];
        assert_eq!(get_most_specific_pattern(&swapped, "ab"), Some("*b"));
    }
}
