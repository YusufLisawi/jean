//! Request transformation pipeline for the AI proxy.
//!
//! Transforms model names with special suffixes into proper API parameters:
//! - `*-thinking-{budget}` -> injects `thinking` block with `budget_tokens`
//! - `*-reasoning-{level}` -> injects `reasoning` block with `effort`
//! - Model group names -> resolved to real model via round-robin

use serde_json::{json, Value};

use super::model_groups::ModelGroupRegistry;

/// Hard cap on thinking budget (VibeProxy limit).
const MAX_THINKING_BUDGET: u64 = 32_000;

/// Minimum headroom added on top of thinking budget for `max_tokens`.
const MIN_TOKEN_HEADROOM: u64 = 1024;

/// Valid reasoning effort levels.
const VALID_REASONING_LEVELS: &[&str] = &["low", "medium", "high"];

/// Result of the transform pipeline.
pub struct TransformResult {
    pub model: String,
    pub body: Value,
    /// Set if model was resolved from a group.
    pub group_name: Option<String>,
    /// Set when a plain model ID was resolved to a provider/model target.
    pub plain_model: Option<String>,
}

/// Apply all transforms to a request body in order:
/// 1. Model group resolution (if model matches a group name)
/// 2. Thinking suffix: `model-thinking-N` -> strip, inject thinking params
/// 3. Reasoning effort: `model-reasoning-LEVEL` -> strip, inject reasoning params
pub fn apply_transforms(body: &mut Value, groups: &ModelGroupRegistry) -> TransformResult {
    let raw_model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // 1. Model group resolution
    let (model, group_name, plain_model) = if has_explicit_provider_model(&raw_model) {
        (raw_model, None, None)
    } else {
        match groups.resolve(&raw_model) {
            Some(resolved) => {
                let target = resolved.target();
                body["model"] = json!(target.clone());
                (target, Some(resolved.group_name), None)
            }
            None => match groups.resolve_plain_model(&raw_model) {
                Some(resolved) => {
                    let target = resolved.target();
                    body["model"] = json!(target.clone());
                    (target, None, Some(raw_model))
                }
                None => (raw_model, None, None),
            },
        }
    };

    // 2. Thinking suffix
    if let Some((base, budget)) = parse_thinking_suffix(&model) {
        let budget = budget.min(MAX_THINKING_BUDGET);
        body["model"] = json!(base);
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": budget,
        });

        // Ensure max_tokens has enough headroom
        let headroom = MIN_TOKEN_HEADROOM.max((budget as f64 * 0.1) as u64);
        let min_max_tokens = budget + headroom;
        let current = body.get("max_tokens").and_then(Value::as_u64).unwrap_or(0);
        if current < min_max_tokens {
            body["max_tokens"] = json!(min_max_tokens);
        }

        return TransformResult {
            model: base,
            body: body.clone(),
            group_name,
            plain_model,
        };
    }

    // 3. Reasoning suffix
    if let Some((base, level)) = parse_reasoning_suffix(&model) {
        body["model"] = json!(base);
        body["reasoning"] = json!({ "effort": level });

        return TransformResult {
            model: base,
            body: body.clone(),
            group_name,
            plain_model,
        };
    }

    TransformResult {
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        body: body.clone(),
        group_name,
        plain_model,
    }
}

/// Parse thinking suffix: `"claude-sonnet-4-thinking-16000"` -> `("claude-sonnet-4", 16000)`
fn parse_thinking_suffix(model: &str) -> Option<(String, u64)> {
    let (prefix, candidate) = model.rsplit_once("-thinking-")?;
    let budget: u64 = candidate.parse().ok()?;
    Some((prefix.to_string(), budget))
}

/// Parse reasoning suffix: `"gpt-5-reasoning-high"` -> `("gpt-5", "high")`
fn parse_reasoning_suffix(model: &str) -> Option<(String, String)> {
    let (prefix, level) = model.rsplit_once("-reasoning-")?;
    if VALID_REASONING_LEVELS.contains(&level) {
        Some((prefix.to_string(), level.to_string()))
    } else {
        None
    }
}

fn has_explicit_provider_model(model: &str) -> bool {
    let Some((provider, target_model)) = model.split_once('/') else {
        return false;
    };
    !provider.trim().is_empty() && !target_model.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_proxy::types::{ProxyModelGroup, ProxyModelGroupMember};

    fn empty_registry() -> ModelGroupRegistry {
        ModelGroupRegistry::new(vec![])
    }

    fn registry_with_groups(groups: Vec<ProxyModelGroup>) -> ModelGroupRegistry {
        ModelGroupRegistry::new(groups)
    }

    fn member(model: &str, provider: &str, enabled: bool) -> ProxyModelGroupMember {
        ProxyModelGroupMember {
            model: model.to_string(),
            provider: provider.to_string(),
            enabled,
        }
    }

    #[test]
    fn thinking_suffix_parsed() {
        let (base, budget) = parse_thinking_suffix("claude-sonnet-4-thinking-16000").unwrap();
        assert_eq!(base, "claude-sonnet-4");
        assert_eq!(budget, 16000);
    }

    #[test]
    fn thinking_suffix_capped() {
        let groups = empty_registry();
        let mut body = json!({"model": "claude-sonnet-4-thinking-99999"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "claude-sonnet-4");
        assert_eq!(body["thinking"]["budget_tokens"], MAX_THINKING_BUDGET);
    }

    #[test]
    fn thinking_sets_max_tokens() {
        let groups = empty_registry();
        let mut body = json!({"model": "claude-sonnet-4-thinking-10000"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "claude-sonnet-4");
        // headroom = max(1024, 10000 * 0.1) = 1024; min_max = 11024
        assert_eq!(body["max_tokens"], 11024);
    }

    #[test]
    fn thinking_preserves_higher_max_tokens() {
        let groups = empty_registry();
        let mut body = json!({"model": "x-thinking-5000", "max_tokens": 50000});
        apply_transforms(&mut body, &groups);
        assert_eq!(body["max_tokens"], 50000);
    }

    #[test]
    fn reasoning_suffix_parsed() {
        let (base, level) = parse_reasoning_suffix("gpt-5-reasoning-high").unwrap();
        assert_eq!(base, "gpt-5");
        assert_eq!(level, "high");
    }

    #[test]
    fn reasoning_invalid_level_ignored() {
        assert!(parse_reasoning_suffix("gpt-5-reasoning-extreme").is_none());
    }

    #[test]
    fn no_suffix_passthrough() {
        let groups = empty_registry();
        let mut body = json!({"model": "claude-sonnet-4"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "claude-sonnet-4");
        assert!(result.group_name.is_none());
    }

    #[test]
    fn explicit_provider_model_is_authoritative() {
        let groups = registry_with_groups(vec![ProxyModelGroup {
            name: "openai/gpt-4o-mini".to_string(),
            models: vec![member("claude-haiku", "claude", true)],
            enabled: true,
            strategy: "round_robin".to_string(),
        }]);
        let mut body = json!({"model": "openai/gpt-4o-mini"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "openai/gpt-4o-mini");
        assert!(result.group_name.is_none());
        assert_eq!(body["model"], "openai/gpt-4o-mini");
    }

    #[test]
    fn group_resolution_sets_provider_model() {
        let groups = registry_with_groups(vec![ProxyModelGroup {
            name: "fast".to_string(),
            models: vec![member("gpt-4o-mini", "openai", true)],
            enabled: true,
            strategy: "round_robin".to_string(),
        }]);
        let mut body = json!({"model": "fast"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "openai/gpt-4o-mini");
        assert_eq!(result.group_name.as_deref(), Some("fast"));
        assert_eq!(body["model"], "openai/gpt-4o-mini");
    }

    #[test]
    fn plain_model_uses_first_provider_match() {
        let groups = registry_with_groups(vec![ProxyModelGroup {
            name: "dupe".to_string(),
            models: vec![
                member("gpt-4o-mini", "openai", true),
                member("gpt-4o-mini", "azure-openai", true),
            ],
            enabled: true,
            strategy: "round_robin".to_string(),
        }]);
        let mut body = json!({"model": "gpt-4o-mini"});
        let result = apply_transforms(&mut body, &groups);
        assert_eq!(result.model, "openai/gpt-4o-mini");
        assert!(result.group_name.is_none());
        assert_eq!(result.plain_model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(body["model"], "openai/gpt-4o-mini");
    }
}
