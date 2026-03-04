//! Model group registry with round-robin selection and failover.
//!
//! Groups map virtual model names to pools of real models across providers.
//! Selection uses atomic cursors for lock-free round-robin.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::types::{ProxyModelGroup, ProxyModelGroupMember};

pub struct ResolvedModel {
    pub group_name: String,
    pub model: String,
    pub provider: String,
}

impl ResolvedModel {
    pub fn target(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

pub struct ModelGroupRegistry {
    groups: Vec<ProxyModelGroup>,
    /// Atomic cursor per group name for round-robin.
    cursors: HashMap<String, AtomicUsize>,
}

impl ModelGroupRegistry {
    pub fn new(groups: Vec<ProxyModelGroup>) -> Self {
        let cursors = groups
            .iter()
            .map(|g| (g.name.clone(), AtomicUsize::new(0)))
            .collect();
        Self { groups, cursors }
    }

    /// Resolve a virtual model name to a real model via round-robin.
    ///
    /// Returns `None` if the name doesn't match any enabled group,
    /// or if the group has no enabled members.
    pub fn resolve(&self, model_name: &str) -> Option<ResolvedModel> {
        let group = self
            .groups
            .iter()
            .find(|g| g.enabled && g.name == model_name)?;

        let enabled: Vec<_> = group.models.iter().filter(|m| m.enabled).collect();
        if enabled.is_empty() {
            return None;
        }

        let cursor = self.cursors.get(model_name)?;
        let idx = cursor.fetch_add(1, Ordering::Relaxed) % enabled.len();
        let member = &enabled[idx];

        resolved_model_from_member(&group.name, member)
    }

    /// Resolve a plain model ID to the first enabled provider match.
    pub fn resolve_plain_model(&self, model_name: &str) -> Option<ResolvedModel> {
        let plain = model_name.trim();
        if plain.is_empty() || plain.contains('/') {
            return None;
        }

        for group in self.groups.iter().filter(|g| g.enabled) {
            for member in group.models.iter().filter(|m| m.enabled) {
                let Some((_, member_model)) = member_provider_model(member) else {
                    continue;
                };
                if member_model == plain {
                    return resolved_model_from_member(&group.name, member);
                }
            }
        }

        None
    }

    /// Advance past failed models and return the next untried one.
    ///
    /// Returns `None` if all enabled models have been tried.
    pub fn failover(&self, group_name: &str, tried: &[String]) -> Option<ResolvedModel> {
        let group = self
            .groups
            .iter()
            .find(|g| g.enabled && g.name == group_name)?;

        group
            .models
            .iter()
            .filter(|m| {
                if !m.enabled {
                    return false;
                }
                let Some((provider, model)) = member_provider_model(m) else {
                    return false;
                };
                let target = format!("{provider}/{model}");
                !tried.contains(&target)
            })
            .find_map(|member| resolved_model_from_member(&group.name, member))
    }

    /// Find the next provider/model target for a plain model ID.
    ///
    /// This keeps plain `model` routing deterministic while still allowing
    /// provider-aware failover when multiple providers expose the same model.
    pub fn failover_plain_model(
        &self,
        model_name: &str,
        tried: &[String],
    ) -> Option<ResolvedModel> {
        let plain = model_name.trim();
        if plain.is_empty() || plain.contains('/') {
            return None;
        }

        for group in self.groups.iter().filter(|g| g.enabled) {
            for member in group.models.iter().filter(|m| m.enabled) {
                let Some((provider, member_model)) = member_provider_model(member) else {
                    continue;
                };

                if member_model != plain {
                    continue;
                }

                let target = format!("{provider}/{member_model}");
                if tried.contains(&target) {
                    continue;
                }

                return Some(ResolvedModel {
                    group_name: group.name.clone(),
                    model: member_model,
                    provider,
                });
            }
        }

        None
    }

    /// Replace all groups (hot-reload from settings).
    pub fn update(&mut self, groups: Vec<ProxyModelGroup>) {
        self.cursors = groups
            .iter()
            .map(|g| (g.name.clone(), AtomicUsize::new(0)))
            .collect();
        self.groups = groups;
    }

    /// Get enabled group names for injecting into `/v1/models` responses.
    pub fn group_names(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter(|g| g.enabled)
            .map(|g| g.name.clone())
            .collect()
    }
}

fn split_provider_model(value: &str) -> Option<(String, String)> {
    let (provider, model) = value.split_once('/')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string()))
}

fn member_provider_model(member: &ProxyModelGroupMember) -> Option<(String, String)> {
    let model = member.model.trim();
    if let Some((provider, model)) = split_provider_model(model) {
        return Some((provider, model));
    }

    let provider = member.provider.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }

    Some((provider.to_string(), model.to_string()))
}

fn resolved_model_from_member(
    group_name: &str,
    member: &ProxyModelGroupMember,
) -> Option<ResolvedModel> {
    let (provider, model) = member_provider_model(member)?;
    Some(ResolvedModel {
        group_name: group_name.to_string(),
        model,
        provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_proxy::types::ProxyModelGroupMember;

    fn member(model: &str, provider: &str, enabled: bool) -> ProxyModelGroupMember {
        ProxyModelGroupMember {
            model: model.to_string(),
            provider: provider.to_string(),
            enabled,
        }
    }

    fn test_group() -> ProxyModelGroup {
        ProxyModelGroup {
            name: "fast".to_string(),
            models: vec![
                member("claude-haiku", "claude", true),
                member("gpt-4o-mini", "openai", true),
                member("disabled-model", "openai", false),
            ],
            enabled: true,
            strategy: "round_robin".to_string(),
        }
    }

    fn duplicate_model_group() -> ProxyModelGroup {
        ProxyModelGroup {
            name: "dupe".to_string(),
            models: vec![
                member("gpt-4o-mini", "openai", true),
                member("gpt-4o-mini", "azure-openai", true),
            ],
            enabled: true,
            strategy: "round_robin".to_string(),
        }
    }

    #[test]
    fn resolve_round_robin() {
        let registry = ModelGroupRegistry::new(vec![test_group()]);
        let r1 = registry.resolve("fast").unwrap();
        let r2 = registry.resolve("fast").unwrap();
        // Should alternate between the two enabled models
        assert_ne!(r1.model, r2.model);
    }

    #[test]
    fn resolve_skips_disabled_models() {
        let registry = ModelGroupRegistry::new(vec![test_group()]);
        for _ in 0..10 {
            let r = registry.resolve("fast").unwrap();
            assert_ne!(r.model, "disabled-model");
        }
    }

    #[test]
    fn resolve_unknown_group_returns_none() {
        let registry = ModelGroupRegistry::new(vec![test_group()]);
        assert!(registry.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolve_disabled_group_returns_none() {
        let mut group = test_group();
        group.enabled = false;
        let registry = ModelGroupRegistry::new(vec![group]);
        assert!(registry.resolve("fast").is_none());
    }

    #[test]
    fn failover_skips_tried() {
        let registry = ModelGroupRegistry::new(vec![test_group()]);
        let next = registry
            .failover("fast", &["claude/claude-haiku".to_string()])
            .unwrap();
        assert_eq!(next.model, "gpt-4o-mini");
    }

    #[test]
    fn failover_returns_none_when_exhausted() {
        let registry = ModelGroupRegistry::new(vec![test_group()]);
        let result = registry.failover(
            "fast",
            &[
                "claude/claude-haiku".to_string(),
                "openai/gpt-4o-mini".to_string(),
            ],
        );
        assert!(result.is_none());
    }

    #[test]
    fn failover_tracks_provider_model_pairs() {
        let registry = ModelGroupRegistry::new(vec![duplicate_model_group()]);
        let next = registry
            .failover("dupe", &["openai/gpt-4o-mini".to_string()])
            .unwrap();
        assert_eq!(next.provider, "azure-openai");
        assert_eq!(next.model, "gpt-4o-mini");
    }

    #[test]
    fn failover_plain_model_skips_tried_provider_model_pair() {
        let registry = ModelGroupRegistry::new(vec![duplicate_model_group()]);
        let next = registry
            .failover_plain_model("gpt-4o-mini", &["openai/gpt-4o-mini".to_string()])
            .unwrap();
        assert_eq!(next.provider, "azure-openai");
        assert_eq!(next.model, "gpt-4o-mini");
    }

    #[test]
    fn plain_model_defaults_to_first_provider_match() {
        let registry = ModelGroupRegistry::new(vec![duplicate_model_group()]);
        let resolved = registry.resolve_plain_model("gpt-4o-mini").unwrap();
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "gpt-4o-mini");
    }

    #[test]
    fn update_resets_cursors() {
        let mut registry = ModelGroupRegistry::new(vec![test_group()]);
        // Advance cursor
        registry.resolve("fast");
        registry.resolve("fast");
        // Update with same groups — cursor should reset
        registry.update(vec![test_group()]);
        let r = registry.resolve("fast").unwrap();
        assert_eq!(r.model, "claude-haiku"); // First model again
    }

    #[test]
    fn group_names_filters_disabled() {
        let mut disabled = test_group();
        disabled.name = "slow".to_string();
        disabled.enabled = false;
        let registry = ModelGroupRegistry::new(vec![test_group(), disabled]);
        let names = registry.group_names();
        assert_eq!(names, vec!["fast"]);
    }
}
