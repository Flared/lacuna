use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct UserAgentMetadata {
    pub raw: String,
    pub normalized: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserAgentPatternConfig {
    pub id: String,
    #[serde(
        deserialize_with = "deserialize_regex",
        serialize_with = "serialize_regex"
    )]
    pub pattern: Regex,
}

impl PartialEq for UserAgentPatternConfig {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.pattern.as_str() == other.pattern.as_str()
    }
}

fn deserialize_regex<'de, D>(deserializer: D) -> Result<Regex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Regex::new(&s).map_err(serde::de::Error::custom)
}

fn serialize_regex<S>(regex: &Regex, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(regex.as_str())
}

struct UserAgentPattern {
    id: String,
    regex: Regex,
}

pub struct UserAgentExtractor {
    patterns: Vec<UserAgentPattern>,
}

impl std::fmt::Debug for UserAgentExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserAgentExtractor")
            .field("patterns_count", &self.patterns.len())
            .finish()
    }
}

impl UserAgentExtractor {
    pub fn new(extra: Vec<UserAgentPatternConfig>) -> Self {
        let mut patterns: Vec<UserAgentPattern> = extra
            .into_iter()
            .map(|c| UserAgentPattern {
                id: c.id,
                regex: c.pattern,
            })
            .collect();

        let defaults = vec![
            ("claude-app", r"(?i)\bClaude/[\d.]+\b.*\bElectron/"),
            ("claude-app", r"(?i)^claude[-_]?cli/.*\blocal-agent\b"),
            ("claude-app", r"(?i)claude[-_]?app|ClaudeDesktop"),
            ("claude-code", r"(?i)claude[-_]?(code|cli)"),
            ("cursor", r"(?i)cursor"),
            ("cline", r"(?i)cline"),
            ("aider", r"(?i)aider"),
            ("continue-dev", r"(?i)continue"),
            ("copilot", r"(?i)copilot"),
        ];

        for (id, pattern) in defaults {
            patterns.push(UserAgentPattern {
                id: id.to_owned(),
                regex: Regex::new(pattern).unwrap(),
            });
        }

        Self { patterns }
    }

    pub fn extract(&self, raw: &str) -> UserAgentMetadata {
        let normalized = self
            .patterns
            .iter()
            .find(|p| p.regex.is_match(raw))
            .map(|p| p.id.clone())
            .unwrap_or_else(|| "unknown".to_owned());

        UserAgentMetadata {
            raw: raw.to_owned(),
            normalized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(id: &str, pattern: &str) -> UserAgentPatternConfig {
        UserAgentPatternConfig {
            id: id.to_owned(),
            pattern: Regex::new(pattern).unwrap(),
        }
    }

    #[test]
    fn test_known_user_agents() {
        let extractor = UserAgentExtractor::new(vec![]);
        let cases = vec![
            ("claude-code/1.0.0", "claude-code"),
            ("ClaudeCode/2.1", "claude-code"),
            ("claude-cli/2.1.68", "claude-code"),
            (
                "claude-cli/2.1.237 (external, local-agent, agent-sdk/0.3.237)",
                "claude-app",
            ),
            ("ClaudeDesktop/1.0", "claude-app"),
            ("claude-app/1.0", "claude-app"),
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Claude/1.34493.1 Chrome/148.0.7778.280 \
                 Electron/42.9.2 Safari/537.36",
                "claude-app",
            ),
            ("Cursor/0.45.0", "cursor"),
            ("cline/3.2.1", "cline"),
            ("Aider/0.50.0", "aider"),
            ("Continue-Dev/1.0", "continue-dev"),
            ("copilot/1.0", "copilot"),
        ];

        for (raw, expected) in cases {
            let meta = extractor.extract(raw);
            assert_eq!(meta.normalized, expected, "failed for raw={raw}");
            assert_eq!(meta.raw, raw);
        }
    }

    #[test]
    fn test_unknown_user_agent() {
        let extractor = UserAgentExtractor::new(vec![]);
        let meta = extractor.extract("Mozilla/5.0");
        assert_eq!(meta.normalized, "unknown");
        assert_eq!(meta.raw, "Mozilla/5.0");
    }

    #[test]
    fn test_custom_pattern_overrides_default() {
        let extractor = UserAgentExtractor::new(vec![config("custom-cursor", r"(?i)cursor")]);

        let meta = extractor.extract("Cursor/0.45.0");
        assert_eq!(meta.normalized, "custom-cursor");
    }

    #[test]
    fn test_invalid_regex_rejected_at_deserialization() {
        let json = r#"{ "id": "bad", "pattern": "[invalid" }"#;
        let err = serde_json::from_str::<UserAgentPatternConfig>(json).unwrap_err();
        assert!(err.to_string().contains("unclosed character class"));
    }
}
