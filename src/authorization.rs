use crate::matching::OnAbsent;
use crate::matching::permissive_match;
use crate::request_metadata::RequestMetadata;

#[derive(Debug, Clone)]
pub struct Rule {
    pub providers: Vec<glob::Pattern>,
    pub model_patterns: Vec<glob::Pattern>,
    pub user_agents: Vec<glob::Pattern>,
}

#[derive(Debug, Clone, Default)]
pub struct Authorization {
    pub rules: Vec<Rule>,
}

impl Authorization {
    pub fn is_allowed(&self, request_metadata: &RequestMetadata) -> bool {
        let provider = &request_metadata.provider_key;
        let user_agent = request_metadata
            .user_agent
            .as_ref()
            .map(|ua| ua.normalized.as_str());

        self.rules.iter().any(|rule| {
            permissive_match(&rule.providers, Some(provider), OnAbsent::Check)
                && permissive_match(
                    &rule.model_patterns,
                    request_metadata.inspected.model.as_deref(),
                    OnAbsent::Ignore,
                )
                && permissive_match(&rule.user_agents, user_agent, OnAbsent::Check)
        })
    }

    pub fn deny_all() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_metadata::RequestInspectionMetadata;

    fn pattern(s: &str) -> glob::Pattern {
        glob::Pattern::new(s).unwrap()
    }

    fn rule(providers: &[&str], models: &[&str]) -> Rule {
        Rule {
            providers: providers.iter().map(|s| pattern(s)).collect(),
            model_patterns: models.iter().map(|s| pattern(s)).collect(),
            user_agents: vec![],
        }
    }

    fn rule_with_user_agents(providers: &[&str], models: &[&str], user_agents: &[&str]) -> Rule {
        Rule {
            providers: providers.iter().map(|s| pattern(s)).collect(),
            model_patterns: models.iter().map(|s| pattern(s)).collect(),
            user_agents: user_agents.iter().map(|s| pattern(s)).collect(),
        }
    }

    fn metadata(provider: &str, model: Option<&str>) -> RequestMetadata {
        RequestMetadata {
            provider_key: provider.to_string(),
            api_handler_id: String::new(),
            user_identity: None,
            user_agent: None,
            inspected: RequestInspectionMetadata {
                model: model.map(|m| m.to_string()),
                ..Default::default()
            },
            labels: std::collections::HashMap::new(),
        }
    }

    fn metadata_with_user_agent(provider: &str, user_agent: Option<&str>) -> RequestMetadata {
        RequestMetadata {
            provider_key: provider.to_string(),
            api_handler_id: String::new(),
            user_identity: None,
            user_agent: user_agent.map(|ua| crate::user_agent::UserAgentMetadata {
                raw: ua.to_string(),
                normalized: ua.to_string(),
            }),
            inspected: RequestInspectionMetadata::default(),
            labels: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_is_allowed() {
        let auth = Authorization {
            rules: vec![
                rule(&["provider-a", "provider-a-*"], &["claude-*", "gpt-4o"]),
                rule(&["provider-b"], &["gpt-3.5-turbo"]),
                rule(&["*"], &["fallback"]),
            ],
        };

        // Provider with Some model (checks both provider and model)
        assert!(auth.is_allowed(&metadata("provider-a", Some("claude-sonnet-4-20250514"))));
        assert!(auth.is_allowed(&metadata("provider-a", Some("gpt-4o"))));
        assert!(!auth.is_allowed(&metadata("provider-a", Some("gpt-3.5-turbo"))));
        assert!(auth.is_allowed(&metadata("provider-a", None)));

        // Wildcard provider with Some model
        assert!(auth.is_allowed(&metadata("provider-a-1", Some("claude-opus-4-20250514"))));

        // Auth can fallback on more permissive rule.
        assert!(auth.is_allowed(&metadata("provider-a-2", Some("fallback"))));
        assert!(auth.is_allowed(&metadata("provider-b", Some("fallback"))));

        // Wrong provider
        assert!(!auth.is_allowed(&metadata("other", Some("claude-sonnet-4-20250514"))));
    }

    #[test]
    fn test_is_allowed_without_model_skips_model_patterns() {
        let auth = Authorization {
            rules: vec![rule(&["provider-a"], &["claude-*"])],
        };

        // A request that carries no model is not filtered on model.
        assert!(auth.is_allowed(&metadata("provider-a", None)));

        // A model that is present must still match.
        assert!(auth.is_allowed(&metadata("provider-a", Some("claude-opus-5"))));
        assert!(!auth.is_allowed(&metadata("provider-a", Some("gpt-4o"))));

        // Other attributes still apply when no model is present.
        assert!(!auth.is_allowed(&metadata("provider-b", None)));
    }

    #[test]
    fn test_is_allowed_empty_list_allows_any() {
        // Empty models list means "all allowed"
        let auth = Authorization {
            rules: vec![rule(&["provider-a"], &[]), rule(&[], &["gpt-4o"])],
        };
        assert!(auth.is_allowed(&metadata("provider-a", Some("anything"))));
        assert!(auth.is_allowed(&metadata("provider-a", None)));
        assert!(!auth.is_allowed(&metadata("provider-b", Some("claude-sonnet-4-20250514"))));
        assert!(auth.is_allowed(&metadata("provider-b", Some("gpt-4o"))));
    }

    #[test]
    fn test_deny_all() {
        let auth = Authorization::deny_all();
        assert!(!auth.is_allowed(&metadata("anything", None)));
        assert!(!auth.is_allowed(&metadata("anything", Some("model"))));
    }

    #[test]
    fn test_is_allowed_user_agent() {
        let auth = Authorization {
            rules: vec![
                rule_with_user_agents(&["provider-a"], &[], &["claude-code", "cursor"]),
                rule_with_user_agents(&["provider-b"], &[], &["*"]),
            ],
        };

        // Allowed user agents
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-a", Some("claude-code"))));
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-a", Some("cursor"))));

        // Denied user agent
        assert!(!auth.is_allowed(&metadata_with_user_agent("provider-a", Some("unknown"))));

        // Missing user agent does not match non-empty list
        assert!(!auth.is_allowed(&metadata_with_user_agent("provider-a", None)));

        // Wildcard matches any user agent
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-b", Some("anything"))));
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-b", None)));
    }

    #[test]
    fn test_is_allowed_empty_user_agent_list_allows_any() {
        let auth = Authorization {
            rules: vec![rule_with_user_agents(&["provider-a"], &[], &[])],
        };
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-a", Some("claude-code"))));
        assert!(auth.is_allowed(&metadata_with_user_agent("provider-a", None)));
    }
}
