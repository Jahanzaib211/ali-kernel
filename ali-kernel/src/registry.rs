//! L6 Brain Proxy Model Registry — per-model cognitive treatment.
//!
//! Each model the gateway can dispatch against has a `ModelSpec` that
//! tells the rest of the pipeline:
//!
//! - whether the model is local (needs kv-cache-saving tricks) or
//!   remote (just proxy),
//! - whether the two-pass think-then-answer loop should fire (small
//!   local models need it; large remote models waste time on it),
//! - whether the constraint mirror should be prepended (small models
//!   lose attention; large models don't),
//! - which wire-format to render tools in (OpenAI, Anthropic, native),
//! - what the typical hidden-reasoning cost is (zero for non-reasoners,
//!   500-1000 for reasoning models — informs the cost-based `max_tokens`
//!   formula in `passes::dispatch`).
//!
//! ## Resolution order
//!
//! `for_model_id_or_default(id)` looks up, in order:
//! 1. The user-overlay table at `ALI_MODELS_TOML` (defaults to
//!    `~/.config/ali-kernel/models.toml`).
//! 2. The baked-in default table shipped with the binary.
//! 3. The hardcoded `[models.default]` profile (always present in the
//!    baked defaults; this is **Registry Fallback** per the v3.1 plan).
//!
//! Unknown models never panic — the gateway continues serving with the
//! conservative `default` profile (assumes `remote_large`,
//! `needs_two_pass = false`, OpenAI tool format).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Coarse model capability tier. Drives the router's temperature +
/// budget heuristics when the model spec doesn't already pin them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelType {
    /// Local small model (<= 13B params), e.g. <9B Q4. Needs two-pass
    /// + constraint mirror; budget is tight.
    LocalSmall,
    /// Local medium model (13-30B). One pass usually enough but
    /// constraint mirror still helps.
    LocalMedium,
    /// Remote small (cheap remote model). One pass; OpenAI format.
    RemoteSmall,
    /// Remote large (GPT-4o, Claude 3.5, etc.). One pass; no mirror.
    RemoteLarge,
    /// Sentinel: the conservative catch-all profile applied when the
    /// requested model id isn't in either the user overlay or the baked
    /// table. Behaves like `RemoteLarge` with OpenAI tool format.
    Unknown,
}

/// Wire format the backend expects for `tools[]` payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFormat {
    /// OpenAI `tools: [{type: "function", function: {name, parameters}}]`
    OpenAI,
    /// Anthropic `tools: [{name, description, input_schema}]`
    Anthropic,
    /// llama.cpp native (today this is the same as OpenAI on most servers).
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    #[serde(default = "default_model_type")]
    pub model_type: ModelType,
    #[serde(default = "default_wire_format")]
    pub tool_format: WireFormat,
    #[serde(default = "default_max_context")]
    pub max_context_tokens: u64,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u64,
    #[serde(default)]
    pub needs_two_pass: bool,
    #[serde(default)]
    pub needs_constraint_mirror: bool,
    #[serde(default)]
    pub typical_thinking: u64,
    #[serde(default = "default_true")]
    pub supports_tools: bool,
}

impl ModelSpec {
    /// The catch-all conservative profile used when a model id is not
    /// found in either table. Treated as remote_large, no two-pass, no
    /// mirror, OpenAI tool format.
    pub fn fallback() -> Self {
        Self {
            model_type: ModelType::RemoteLarge,
            tool_format: WireFormat::OpenAI,
            max_context_tokens: 32_000,
            max_output_tokens: 4_096,
            needs_two_pass: false,
            needs_constraint_mirror: false,
            typical_thinking: 0,
            supports_tools: true,
        }
    }
}

fn default_model_type() -> ModelType {
    ModelType::Unknown
}
fn default_wire_format() -> WireFormat {
    WireFormat::OpenAI
}
fn default_max_context() -> u64 {
    32_000
}
fn default_max_output() -> u64 {
    4_096
}
fn default_true() -> bool {
    true
}

/// Lookup table for model metadata. Loaded once at startup; immutable
/// thereafter.
#[allow(dead_code)] // `for_model_id_or_default` and `default_spec` are
                    // called by the integration commit (Pillar 3+).
pub struct ModelRegistry {
    by_id: HashMap<String, ModelSpec>,
    default_spec: ModelSpec,
    /// Path the user-overlay table was loaded from (for telemetry).
    pub user_overlay_path: Option<PathBuf>,
}

/// Canonical id baked in for the gateway's local flagship model. The
/// runtime deployment may alias this id (e.g. via ALI_MODELS_TOML);
/// the canonical id here is the fallback the gateway guarantees to
/// resolve.
pub const LOCAL_DEFAULT_ID: &str = "local-small-default";

impl ModelRegistry {
    /// Load with the user-overlay path (typically ALI_MODELS_TOML).
    /// Always succeeds: missing file, parse error, or empty overlay
    /// all fall back to the baked defaults.
    pub fn load(user_overlay_path: &Path) -> Self {
        let overlay = read_user_overlay(user_overlay_path);
        let mut by_id = baked_defaults();
        // Field-level merge: the user overlay's partial specs fold
        // into the baked specs; nothing is overwritten unless the
        // user explicitly set it.
        for (k, partial) in overlay {
            let mut base = by_id.get(&k).cloned().unwrap_or_else(ModelSpec::fallback);
            partial.merge_into(&mut base);
            by_id.insert(k, base);
        }
        // The `default` profile is always present (baked in). If the
        // user overlay damaged it, restore from the hardcoded fallback.
        let default_spec = by_id
            .get("default")
            .cloned()
            .unwrap_or_else(ModelSpec::fallback);
        by_id
            .entry("default".into())
            .or_insert(default_spec.clone());
        // The flagship local model is always present (baked in).
        by_id
            .entry(LOCAL_DEFAULT_ID.into())
            .or_insert(baked_local_small_default());
        ModelRegistry {
            by_id,
            default_spec,
            user_overlay_path: Some(user_overlay_path.to_path_buf()),
        }
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.by_id.get(id)
    }

    /// Alias a model id (typically the runtime `backend_model` from
    /// `.env`) to the local flagship spec. Convenient for deployments
    /// where the operator's chosen runtime id isn't the canonical
    /// `local-small-default`. After this call, lookup-by-id for the
    /// alias returns the same spec as the canonical id.
    pub fn alias_local_default(&mut self, alias: &str) {
        if alias.is_empty() || alias == LOCAL_DEFAULT_ID {
            return;
        }
        if let Some(local) = self.by_id.get(LOCAL_DEFAULT_ID).cloned() {
            self.by_id.insert(alias.to_string(), local);
        }
    }

    /// **Registry Fallback**: if `id` is unknown, returns the `default`
    /// profile (never panics, never returns None). Operators get the
    /// conservative treatment and can refine by adding `models.toml`
    /// entries later.
    pub fn for_model_id_or_default(&self, id: &str) -> ModelSpec {
        self.by_id
            .get(id)
            .cloned()
            .unwrap_or_else(|| self.default_spec.clone())
    }

    pub fn default_spec(&self) -> &ModelSpec {
        &self.default_spec
    }

    pub fn ids(&self) -> Vec<String> {
        let mut out: Vec<String> = self.by_id.keys().cloned().collect();
        out.sort();
        out
    }
}

fn read_user_overlay(path: &Path) -> HashMap<String, PartialModelSpec> {
    match std::fs::read_to_string(path) {
        Ok(s) => match toml::from_str::<HashMap<String, PartialModelSpec>>(&s) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "models.toml parse failed at {}: {e}; continuing with baked defaults",
                    path.display()
                );
                HashMap::new()
            }
        },
        Err(_) => HashMap::new(), // missing file is not an error
    }
}

/// Field-level overlay: every `Option` lets the user pick which fields to
/// override; merge with the baked spec at lookup time. Semantics:
/// "operator can toggle a single flag (e.g. `needs_two_pass = false`)
/// without resetting every other field."
#[derive(Debug, Default, Clone, Deserialize)]
pub struct PartialModelSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<ModelType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_format: Option<WireFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_two_pass: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_constraint_mirror: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typical_thinking: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
}

impl PartialModelSpec {
    fn merge_into(self, base: &mut ModelSpec) {
        if let Some(v) = self.model_type {
            base.model_type = v;
        }
        if let Some(v) = self.tool_format {
            base.tool_format = v;
        }
        if let Some(v) = self.max_context_tokens {
            base.max_context_tokens = v;
        }
        if let Some(v) = self.max_output_tokens {
            base.max_output_tokens = v;
        }
        if let Some(v) = self.needs_two_pass {
            base.needs_two_pass = v;
        }
        if let Some(v) = self.needs_constraint_mirror {
            base.needs_constraint_mirror = v;
        }
        if let Some(v) = self.typical_thinking {
            base.typical_thinking = v;
        }
        if let Some(v) = self.supports_tools {
            base.supports_tools = v;
        }
    }
}

fn baked_defaults() -> HashMap<String, ModelSpec> {
    let mut m = HashMap::new();
    m.insert(LOCAL_DEFAULT_ID.into(), baked_local_small_default());
    m.insert(
        "gpt-4o".into(),
        ModelSpec {
            model_type: ModelType::RemoteLarge,
            tool_format: WireFormat::OpenAI,
            max_context_tokens: 128_000,
            max_output_tokens: 16_384,
            needs_two_pass: false,
            needs_constraint_mirror: false,
            typical_thinking: 0,
            supports_tools: true,
        },
    );
    m.insert(
        "claude-3-5-sonnet-20241022".into(),
        ModelSpec {
            model_type: ModelType::RemoteLarge,
            tool_format: WireFormat::Anthropic,
            max_context_tokens: 200_000,
            max_output_tokens: 8_192,
            needs_two_pass: false,
            needs_constraint_mirror: false,
            typical_thinking: 0,
            supports_tools: true,
        },
    );
    m.insert(
        "claude-3-5-haiku-20241022".into(),
        ModelSpec {
            model_type: ModelType::RemoteLarge,
            tool_format: WireFormat::Anthropic,
            max_context_tokens: 200_000,
            max_output_tokens: 8_192,
            needs_two_pass: false,
            needs_constraint_mirror: false,
            typical_thinking: 0,
            supports_tools: true,
        },
    );
    m.insert("default".into(), ModelSpec::fallback());
    m
}

fn baked_local_small_default() -> ModelSpec {
    ModelSpec {
        model_type: ModelType::LocalSmall,
        tool_format: WireFormat::OpenAI,
        max_context_tokens: 12_000,
        max_output_tokens: 4_096,
        needs_two_pass: true,
        needs_constraint_mirror: true,
        typical_thinking: 500,
        supports_tools: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> ModelRegistry {
        ModelRegistry::load(Path::new("/nonexistent/models.toml"))
    }

    #[test]
    fn baked_defaults_present() {
        let r = fresh();
        let ids = r.ids();
        assert!(ids.contains(&LOCAL_DEFAULT_ID.into()), "ids: {ids:?}");
        assert!(ids.contains(&"gpt-4o".into()), "ids: {ids:?}");
        assert!(ids.contains(&"default".into()), "ids: {ids:?}");
        assert!(
            ids.contains(&"claude-3-5-sonnet-20241022".into()),
            "ids: {ids:?}"
        );
    }

    #[test]
    fn registry_fallback_returns_default_for_unknown() {
        let r = fresh();
        let spec = r.for_model_id_or_default("gpt-99-fake");
        assert_eq!(spec.model_type, ModelType::RemoteLarge);
        assert_eq!(spec.tool_format, WireFormat::OpenAI);
        assert!(!spec.needs_two_pass);
        assert!(!spec.needs_constraint_mirror);
        assert_eq!(spec.max_context_tokens, 32_000);
        // And critically: default returned for LOCAL_DEFAULT_ID is the
        // local-small spec, NOT the fallback (this is the
        // regression-prevention test).
        let spec2 = r.for_model_id_or_default(LOCAL_DEFAULT_ID);
        assert_eq!(spec2.model_type, ModelType::LocalSmall);
        assert!(spec2.needs_two_pass);
        assert!(spec2.needs_constraint_mirror);
    }

    #[test]
    fn user_overlay_partial_merge_preserves_other_fields() {
        let dir = std::env::temp_dir().join("ali-kernel-registry-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.toml");
        // TOML section header for keys containing `-` must be quoted;
        // use the canonical id verbatim.
        std::fs::write(
            &path,
            format!(
                "['{}']\nneeds_two_pass = false\nneeds_constraint_mirror = false\n",
                LOCAL_DEFAULT_ID
            ),
        )
        .unwrap();
        let r = ModelRegistry::load(&path);
        let spec = r.for_model_id_or_default(LOCAL_DEFAULT_ID);
        // Operator flipped two flags.
        assert!(!spec.needs_two_pass, "overlay didn't override");
        assert!(!spec.needs_constraint_mirror);
        // Other baked fields preserved through field-level merge.
        assert_eq!(spec.model_type, ModelType::LocalSmall);
        assert_eq!(spec.tool_format, WireFormat::OpenAI);
        assert_eq!(spec.max_context_tokens, 12_000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn malformed_user_overlay_falls_back_cleanly() {
        let dir = std::env::temp_dir().join("ali-kernel-registry-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("models.toml");
        std::fs::write(&path, "this is not valid toml = = = = [[[").unwrap();
        let r = ModelRegistry::load(&path);
        // No panic, baked defaults preserved.
        let spec = r.for_model_id_or_default("gpt-4o");
        assert_eq!(spec.model_type, ModelType::RemoteLarge);
        let _ = std::fs::remove_file(&path);
    }
}
