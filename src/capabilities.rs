use crate::matching::OnAbsent;
use crate::matching::permissive_match;
use crate::model_rules::ModelRule;
use serde::Deserialize;
use serde::Serialize;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use std::collections::HashMap;

#[derive(PartialEq, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    pub grants: Vec<Grant>,
    pub labels: HashMap<String, String>,
}

#[derive(PartialEq, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grant {
    #[serde(
        serialize_with = "crate::serde_utils::serialize_patterns",
        deserialize_with = "crate::serde_utils::deserialize_patterns"
    )]
    pub providers: Vec<glob::Pattern>,

    #[serde(
        rename = "models",
        default,
        serialize_with = "crate::model_rules::serialize_model_rules",
        deserialize_with = "crate::model_rules::deserialize_model_rules"
    )]
    pub model_rules: Vec<ModelRule>,

    #[serde(
        serialize_with = "crate::serde_utils::serialize_patterns",
        deserialize_with = "crate::serde_utils::deserialize_patterns"
    )]
    pub user_agents: Vec<glob::Pattern>,
}

impl Grant {
    pub fn matches_provider_and_user_agent(
        &self,
        provider_key: &str,
        user_agent: Option<&str>,
    ) -> bool {
        permissive_match(&self.providers, Some(provider_key), OnAbsent::Check)
            && permissive_match(&self.user_agents, user_agent, OnAbsent::Check)
    }
}

impl Capabilities {
    pub fn deny_all() -> Self {
        Self {
            grants: vec![],
            labels: HashMap::new(),
        }
    }

    pub fn collect_model_rules(
        &self,
        provider_key: &str,
        user_agent: Option<&str>,
    ) -> Vec<ModelRule> {
        self.grants
            .iter()
            .filter(|g| g.matches_provider_and_user_agent(provider_key, user_agent))
            .flat_map(|g| g.model_rules.clone())
            .collect()
    }
}

impl From<Capabilities> for crate::authorization::Authorization {
    fn from(caps: Capabilities) -> Self {
        Self {
            rules: caps
                .grants
                .into_iter()
                .map(|g| crate::authorization::Rule {
                    providers: g.providers,
                    model_patterns: g.model_rules.into_iter().map(|r| r.pattern).collect(),
                    user_agents: g.user_agents,
                })
                .collect(),
        }
    }
}

// Tailscale capabilities are a JSON object where keys are capability
// names and values are arrays of arbitrary JSON objects.
//
// Ref: https://tailscale.com/docs/features/access-control/grants/grants-app-capabilities
#[serde_as]
#[derive(Debug, Deserialize)]
struct TailscaleCapabilities {
    #[serde_as(as = "Vec<DefaultOnError<Option<_>>>")]
    #[serde(default, rename = "flare.io/cap/lacuna/grants")]
    grants: Vec<Option<Grant>>,

    #[serde_as(as = "Vec<DefaultOnError<Option<_>>>")]
    #[serde(default, rename = "flare.io/cap/lacuna/labels")]
    labels: Vec<Option<HashMap<String, String>>>,
}

pub fn parse_capabilities(header_value: &str) -> Result<Capabilities, anyhow::Error> {
    let decoded = rfc2047_decoder::Decoder::new()
        .too_long_encoded_word_strategy(rfc2047_decoder::RecoverStrategy::Decode)
        .decode(header_value.as_bytes())
        .unwrap_or_else(|_| header_value.to_owned());

    let ts_capabilities: TailscaleCapabilities = match serde_json::from_str(&decoded) {
        Ok(v) => v,
        Err(e) => {
            return Err(anyhow::anyhow!("failed to parse capabilities header: {e}"));
        }
    };

    let capabilities = Capabilities {
        grants: ts_capabilities.grants.into_iter().flatten().collect(),
        labels: {
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            // Collect labels from list of capabilities.
            for (k, v) in ts_capabilities
                .labels
                .into_iter()
                .flatten()
                .flat_map(|m| m.into_iter())
            {
                map.entry(k).or_default().push(v);
            }
            // Merge label with the same key with a ",".join
            map.into_iter()
                .map(|(k, mut vs)| {
                    vs.sort();
                    (k, vs.join(","))
                })
                .collect()
        },
    };

    Ok(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(s: &str) -> glob::Pattern {
        glob::Pattern::new(s).unwrap()
    }

    fn model_rule(s: &str, rewrite: Option<&str>) -> ModelRule {
        ModelRule {
            pattern: pattern(s),
            rewrite: rewrite.map(|r| r.to_owned()),
        }
    }

    #[test]
    fn test_grant_deserialize() {
        let json = r#"{
                "providers": ["myprovider", "prefix-*"], 
                "models": {
                    "claude-*": {},
                    "model": {
                        "rewrite": "other-model"
                    }
                }, 
                "user_agents": ["python-*"]
            }"#;
        let grant: Grant = serde_json::from_str(json).unwrap();
        assert_eq!(
            grant,
            Grant {
                providers: vec![pattern("myprovider"), pattern("prefix-*")],
                model_rules: vec![
                    model_rule("claude-*", None),
                    model_rule("model", Some("other-model"))
                ],
                user_agents: vec![pattern("python-*")],
            },
        );
    }

    #[test]
    fn test_grant_deserialize_no_models() {
        let json = r#"{"providers": ["myprovider"]}"#;
        let grant: Grant = serde_json::from_str(json).unwrap();
        assert_eq!(
            grant,
            Grant {
                providers: vec![pattern("myprovider")],
                model_rules: vec![],
                user_agents: vec![],
            },
        );
    }

    #[test]
    fn test_grant_deserialize_invalid_pattern() {
        let json = r#"{"providers": ["valid", "[invalid"]}"#;
        let result: Result<Grant, _> = serde_json::from_str(json);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Pattern syntax error")
        );
    }

    #[test]
    fn test_grant_serialize() {
        let grant = Grant {
            providers: vec![pattern("myprovider"), pattern("prefix-*")],
            model_rules: vec![model_rule("claude-*", Some("target"))],
            user_agents: vec![pattern("python-*")],
        };
        let json = serde_json::to_value(&grant).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "providers": ["myprovider", "prefix-*"],
                "models": {"claude-*": {"rewrite": "target"}},
                "user_agents": ["python-*"],
            }),
        );
    }

    #[test]
    fn parse_valid_capabilities() {
        let json = r#"{
            "flare.io/cap/lacuna/grants": [
                {"providers": ["firstprovider"], "models": {"claude-*": {}}, "user_agents": ["python-*"]},
                {"providers": ["secondprofider", "thirdprovider"], "models": {"gpt-*": {}}},
                {"providers": ["fourthprovider"], "models": {"gemini-*": {"rewrite": "opus"}}}
            ],
            "flare.io/cap/lacuna/labels": [
                {"team": "platform", "env": "production"},
                {"team": "infra"}
            ]
        }"#;
        let capabilities = parse_capabilities(json).unwrap();
        assert_eq!(
            capabilities,
            Capabilities {
                grants: vec![
                    Grant {
                        providers: vec![pattern("firstprovider")],
                        model_rules: vec![model_rule("claude-*", None)],
                        user_agents: vec![pattern("python-*")],
                    },
                    Grant {
                        providers: vec![pattern("secondprofider"), pattern("thirdprovider")],
                        model_rules: vec![model_rule("gpt-*", None)],
                        user_agents: vec![],
                    },
                    Grant {
                        providers: vec![pattern("fourthprovider")],
                        model_rules: vec![model_rule("gemini-*", Some("opus"))],
                        user_agents: vec![],
                    },
                ],
                labels: HashMap::from([
                    ("team".to_owned(), "infra,platform".to_owned()),
                    ("env".to_owned(), "production".to_owned()),
                ]),
            }
        );
    }

    #[test]
    fn parse_capabilities_invalid_ignored() {
        let json = r#"{
            "flare.io/cap/lacuna/grants": [
                {"providers": ["firstprovider"], "models": {"claude-*": {}}},
                ["something-bad"],
                {"providers": ["secondprovider"]},
                {"providers": ["thirdprovider"], "models": {"gemini-*": {"invalid-key": "something"}}}
            ]
        }"#;
        let capabilities = parse_capabilities(json).unwrap();
        assert_eq!(
            capabilities,
            Capabilities {
                grants: vec![
                    Grant {
                        providers: vec![pattern("firstprovider")],
                        model_rules: vec![model_rule("claude-*", None)],
                        user_agents: vec![],
                    },
                    Grant {
                        providers: vec![pattern("secondprovider")],
                        model_rules: vec![],
                        user_agents: vec![],
                    },
                ],
                labels: Default::default(),
            }
        );
    }

    #[test]
    fn parse_missing_key_returns_none() {
        let json = r#"{"other.key": []}"#;
        let capabilities = parse_capabilities(json).unwrap();
        assert_eq!(capabilities, Capabilities::default());
    }

    #[test]
    fn parse_malformed_json_returns_err() {
        assert!(parse_capabilities("not json").is_err());
    }

    #[test]
    fn authorization_from_capabilities_evaluate_non_rewritten_model_name() {
        let caps = Capabilities {
            grants: vec![Grant {
                providers: vec![pattern("myprovider")],
                model_rules: vec![
                    model_rule("claude-*", Some("rewriten-model")),
                    model_rule("gpt-*", None),
                ],
                user_agents: vec![pattern("python-*")],
            }],
            labels: HashMap::new(),
        };
        let auth: crate::authorization::Authorization = caps.into();
        assert_eq!(auth.rules.len(), 1);
        let rule = &auth.rules[0];
        assert_eq!(rule.providers, vec![pattern("myprovider")]);
        assert_eq!(rule.user_agents, vec![pattern("python-*")]);

        // No 'rewriten-model'
        assert_eq!(
            rule.model_patterns,
            vec![pattern("claude-*"), pattern("gpt-*")]
        );
    }

    #[test]
    fn grants_matches_provider_and_user_agent() {
        let grant = Grant {
            providers: vec![pattern("myprovider")],
            model_rules: vec![model_rule("irrelevant-model", None)],
            user_agents: vec![pattern("python-*")],
        };
        assert!(grant.matches_provider_and_user_agent("myprovider", Some("python-requests")));

        // Provider mismatch.
        assert!(!grant.matches_provider_and_user_agent("otherprovider", Some("python-requests")));

        // User agent mismatch.
        assert!(!grant.matches_provider_and_user_agent("myprovider", Some("curl")));

        // Missing user agent does not match a non-empty list.
        assert!(!grant.matches_provider_and_user_agent("myprovider", None));
    }

    #[test]
    fn grants_matches_all_provider_and_user_agent_when_empty_lists() {
        let grant = Grant::default();
        assert!(grant.matches_provider_and_user_agent("anyprovider", Some("any-agent")));
        assert!(grant.matches_provider_and_user_agent("anyprovider", None));
        assert!(grant.matches_provider_and_user_agent("anotherprovider", None));
        assert!(grant.matches_provider_and_user_agent("anotherprovider", Some("another-agent")));
    }

    #[test]
    fn parse_rfc2047_encoded() {
        // Tailscale uses RFC2047 "Q" encoding for values that contain non-ASCII characters.
        // Ref: https://tailscale.com/docs/features/tailscale-serve#app-capabilities-header
        // Q-encoded: {"flare.io/cap/lacuna/grants":[{"providers":["🐿️"]}]}
        let encoded = r#"=?utf-8?q?{"flare.io/cap/lacuna/grants":[{"providers":["=F0=9F=90=BF=EF=B8=8F"]}]}?="#;
        let capabilities = parse_capabilities(encoded).unwrap();
        assert_eq!(
            capabilities,
            Capabilities {
                grants: vec![Grant {
                    providers: vec![pattern("🐿️")],
                    model_rules: vec![],
                    user_agents: vec![],
                },],
                labels: Default::default()
            },
        );
    }
}
