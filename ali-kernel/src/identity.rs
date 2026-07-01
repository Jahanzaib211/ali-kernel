//! Identity — Prefrontal Cortex: model of who you are
//!
//! Nature psychology: The prefrontal cortex holds your self-model — preferences,
//! patterns, values, how you communicate. The identity system is what makes the
//! agent feel like it *knows you*, not just your code.
//!
//! Identity is learned through two channels:
//! - STATED: explicit user corrections/preferences (/pref set, /identity edit)
//! - OBSERVED: inferred from patterns in your behavior (lower confidence)
//!
//! All inferred preferences are tagged as such and can be overridden.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// Who you are
    pub profile: Profile,
    /// Your stated and inferred preferences
    pub preferences: PreferenceMap,
    /// Recurring solution patterns you've used
    pub patterns: PatternMap,
    /// Things you never want
    pub dealbreakers: Vec<DealBreaker>,
    /// Writing voice samples
    pub voice: Voice,
    /// Tech you love / avoid
    pub toolkit: Toolkit,
    /// Last time identity was updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    pub name: Option<String>,
    pub role: Option<String>,
    pub timezone: Option<String>,
    pub pronouns: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreferenceMap {
    /// Stated preferences (user explicitly set)
    pub stated: HashMap<String, Preference>,
    /// Inferred preferences (observed from behavior)
    pub inferred: HashMap<String, Preference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub value: String,
    pub confidence: f32,
    pub source: PreferenceSource,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceSource {
    /// User set it explicitly via /pref set
    Stated,
    /// Inferred from observing behavior
    Observed,
    /// Corrected by user after being inferred
    Corrected,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PatternMap {
    pub error_handling: Vec<String>,
    pub api_design: Vec<String>,
    pub naming: Vec<String>,
    pub testing: Vec<String>,
    pub other: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DealBreaker {
    pub description: String,
    pub reason: String,
    pub trigger_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Voice {
    /// Writing samples (how you explain things)
    pub samples: Vec<VoiceSample>,
    /// How you like code formatted
    pub code_style: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSample {
    pub text: String,
    pub context: String,
    pub ts: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Toolkit {
    pub loves: Vec<String>,
    pub avoids: Vec<String>,
    pub always_uses: Vec<String>,
    pub never_uses: Vec<String>,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            preferences: PreferenceMap::default(),
            patterns: PatternMap::default(),
            dealbreakers: Vec::new(),
            voice: Voice::default(),
            toolkit: Toolkit::default(),
            updated_at: chrono::Utc::now(),
        }
    }
}

// ── Identity Store ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct IdentityStore {
    dir: PathBuf,
    /// In-memory cache
    cache: Arc<RwLock<Option<Identity>>>,
}

impl IdentityStore {
    /// Open identity at `~/.ali/identity/`
    pub fn new(config: &crate::config::Config) -> Result<Self> {
        let dir = config.ali_dir.join("identity");
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            cache: Arc::new(RwLock::new(None)),
        })
    }

    /// Load identity from disk into cache.
    pub async fn load(&self) -> Result<Identity> {
        let path = self.dir.join("identity.json");
        let identity = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            Identity::default()
        };
        *self.cache.write().await = Some(identity.clone());
        tracing::info!("IDENTITY loaded");
        Ok(identity)
    }

    /// Get the cached identity (must call load() first).
    #[allow(dead_code)] // reserved for an /identity endpoint, not wired up yet
    pub async fn get(&self) -> Option<Identity> {
        self.cache.read().await.clone()
    }

    /// Get a specific preference value.
    #[allow(dead_code)] // reserved for /pref, /identity commands, not wired up yet (exercised by tests below)
    pub async fn get_preference(&self, key: &str) -> Option<Preference> {
        let guard = self.cache.read().await;
        if let Some(ref identity) = *guard {
            if let Some(p) = identity.preferences.stated.get(key) {
                return Some(p.clone());
            }
            if let Some(p) = identity.preferences.inferred.get(key) {
                return Some(p.clone());
            }
        }
        None
    }

    /// Set a stated preference (explicit user choice).
    #[allow(dead_code)] // reserved for /pref, /identity commands, not wired up yet (exercised by tests below)
    pub async fn set_preference(&self, key: String, value: String) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        let pref = Preference {
            value,
            confidence: 1.0,
            source: PreferenceSource::Stated,
            updated_at: chrono::Utc::now(),
            examples: Vec::new(),
        };
        identity.preferences.stated.insert(key, pref);
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        tracing::info!(
            "IDENTITY pref set: {} (stated)",
            identity.preferences.stated.len()
        );
        Ok(())
    }

    /// Infer a preference from observed behavior.
    #[allow(dead_code)] // reserved for /pref, /identity commands, not wired up yet (exercised by tests below)
    pub async fn infer_preference(
        &self,
        key: String,
        value: String,
        examples: Vec<String>,
    ) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        // Don't overwrite stated preferences
        if identity.preferences.stated.contains_key(&key) {
            return Ok(());
        }
        let existing = identity.preferences.inferred.get(&key);
        let new_confidence = if let Some(existing) = existing {
            // Accumulate confidence with each observation
            (existing.confidence + 0.15).min(0.85)
        } else {
            0.3
        };
        let pref = Preference {
            value,
            confidence: new_confidence,
            source: PreferenceSource::Observed,
            updated_at: chrono::Utc::now(),
            examples,
        };
        identity.preferences.inferred.insert(key, pref);
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        Ok(())
    }

    /// Correct an inferred preference (user overrides it).
    #[allow(dead_code)] // reserved for /pref, /identity commands, not wired up yet (exercised by tests below)
    pub async fn correct_preference(&self, key: String, value: String) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        let pref = Preference {
            value,
            confidence: 1.0,
            source: PreferenceSource::Corrected,
            updated_at: chrono::Utc::now(),
            examples: Vec::new(),
        };
        // Move from inferred to stated
        identity.preferences.inferred.remove(&key);
        identity.preferences.stated.insert(key, pref);
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        tracing::info!("IDENTITY pref corrected by user");
        Ok(())
    }

    /// Add a dealbreaker.
    #[allow(dead_code)] // reserved for /pref, /identity commands, not wired up yet (exercised by tests below)
    pub async fn add_dealbreaker(
        &self,
        description: String,
        reason: String,
        triggers: Vec<String>,
    ) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        identity.dealbreakers.push(DealBreaker {
            description,
            reason,
            trigger_patterns: triggers,
        });
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        Ok(())
    }

    /// Add a pattern to a category.
    #[allow(dead_code)] // reserved for identity-shaping tools, not wired up yet
    pub async fn add_pattern(&self, category: &str, pattern: String) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        match category {
            "error_handling" => identity.patterns.error_handling.push(pattern),
            "api_design" => identity.patterns.api_design.push(pattern),
            "naming" => identity.patterns.naming.push(pattern),
            "testing" => identity.patterns.testing.push(pattern),
            _ => identity.patterns.other.push(pattern),
        }
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        Ok(())
    }

    /// Add a voice sample.
    #[allow(dead_code)] // reserved for identity-shaping tools, not wired up yet
    pub async fn add_voice_sample(&self, text: String, context: String) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        identity.voice.samples.push(VoiceSample {
            text,
            context,
            ts: chrono::Utc::now(),
        });
        // Keep last 10 samples
        if identity.voice.samples.len() > 10 {
            identity.voice.samples.remove(0);
        }
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        Ok(())
    }

    /// Update profile fields.
    #[allow(dead_code)] // reserved for identity-shaping tools, not wired up yet
    pub async fn update_profile(
        &self,
        name: Option<String>,
        role: Option<String>,
        timezone: Option<String>,
    ) -> Result<()> {
        let mut guard = self.cache.write().await;
        let identity = guard.get_or_insert_with(Identity::default);
        if let Some(n) = name {
            identity.profile.name = Some(n);
        }
        if let Some(r) = role {
            identity.profile.role = Some(r);
        }
        if let Some(tz) = timezone {
            identity.profile.timezone = Some(tz);
        }
        identity.updated_at = chrono::Utc::now();
        self.save_locked(&*identity).await?;
        Ok(())
    }

    /// Inject relevant identity context into the system prompt.
    pub async fn inject_context(&self) -> String {
        let guard = self.cache.read().await;
        let identity = match &*guard {
            Some(i) => i,
            None => return String::new(),
        };

        let mut ctx = String::new();

        // Profile
        if let Some(ref name) = identity.profile.name {
            ctx.push_str(&format!("User name: {}\n", name));
        }
        if let Some(ref role) = identity.profile.role {
            ctx.push_str(&format!("User role: {}\n", role));
        }

        // Stated preferences (high confidence — always relevant)
        if !identity.preferences.stated.is_empty() {
            ctx.push_str("\nYour stated preferences:\n");
            for (key, pref) in &identity.preferences.stated {
                ctx.push_str(&format!("  {}: {}\n", key, pref.value));
            }
        }

        // Inferred preferences (medium confidence — only top ones)
        if !identity.preferences.inferred.is_empty() {
            let top_inferred: Vec<_> = identity
                .preferences
                .inferred
                .iter()
                .filter(|(_, p)| p.confidence >= 0.5)
                .collect();
            if !top_inferred.is_empty() {
                ctx.push_str("\nYour inferred preferences (you may correct these):\n");
                for (key, pref) in top_inferred {
                    ctx.push_str(&format!(
                        "  {}: {} (confidence: {:.0}%)\n",
                        key,
                        pref.value,
                        pref.confidence * 100.0
                    ));
                }
            }
        }

        // Dealbreakers (critical — never violate)
        if !identity.dealbreakers.is_empty() {
            ctx.push_str("\nCRITICAL — Never do these:\n");
            for db in &identity.dealbreakers {
                ctx.push_str(&format!("  - {} ({})\n", db.description, db.reason));
            }
        }

        // Patterns
        if !identity.patterns.error_handling.is_empty() {
            ctx.push_str("\nYour error-handling patterns:\n");
            for p in &identity.patterns.error_handling {
                ctx.push_str(&format!("  - {}\n", p));
            }
        }

        // Toolkit
        if !identity.toolkit.loves.is_empty() {
            ctx.push_str(&format!(
                "\nYou prefer: {}\n",
                identity.toolkit.loves.join(", ")
            ));
        }
        if !identity.toolkit.never_uses.is_empty() {
            ctx.push_str(&format!(
                "You never use: {}\n",
                identity.toolkit.never_uses.join(", ")
            ));
        }

        // Voice (writing style)
        if let Some(ref style) = identity.voice.code_style {
            ctx.push_str(&format!("\nYour code style: {}\n", style));
        }
        if !identity.voice.samples.is_empty() {
            let last = identity.voice.samples.last().unwrap();
            if last.text.len() < 300 {
                ctx.push_str(&format!("\nRecent voice sample: \"{}\"\n", last.text));
            }
        }

        ctx
    }

    // ── Private ──────────────────────────────────────────────────────────────

    async fn save_locked(&self, identity: &Identity) -> Result<()> {
        let path = self.dir.join("identity.json");
        let content = serde_json::to_string_pretty(identity)?;
        fs::write(&path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_identity() -> IdentityStore {
        // Unique dir per call — these tests run in parallel and previously
        // shared one fixed path, racing on the same identity.json file.
        let dir = std::env::temp_dir().join(format!("ali_identity_test_{}", uuid::Uuid::new_v4()));
        let cfg = crate::config::Config {
            ali_dir: dir,
            ..Default::default()
        };
        IdentityStore::new(&cfg).unwrap()
    }

    #[tokio::test]
    async fn set_and_get_preference() {
        let id = temp_identity();
        id.load().await.unwrap();
        id.set_preference("indent".into(), "4 spaces".into())
            .await
            .unwrap();
        let pref = id.get_preference("indent").await.unwrap();
        assert_eq!(pref.value, "4 spaces");
        assert_eq!(pref.confidence, 1.0);
    }

    #[tokio::test]
    async fn inferred_does_not_overwrite_stated() {
        let id = temp_identity();
        id.load().await.unwrap();
        id.set_preference("indent".into(), "2 spaces".into())
            .await
            .unwrap();
        id.infer_preference("indent".into(), "4 spaces".into(), vec![])
            .await
            .unwrap();
        let pref = id.get_preference("indent").await.unwrap();
        assert_eq!(pref.value, "2 spaces"); // stated wins
    }

    #[tokio::test]
    async fn correct_preference() {
        let id = temp_identity();
        id.load().await.unwrap();
        id.infer_preference(
            "style".into(),
            "functional".into(),
            vec!["uses iterators".into()],
        )
        .await
        .unwrap();
        id.correct_preference("style".into(), "OO".into())
            .await
            .unwrap();
        let pref = id.get_preference("style").await.unwrap();
        assert_eq!(pref.value, "OO");
        assert_eq!(pref.source, PreferenceSource::Corrected);
    }

    #[tokio::test]
    async fn inject_context_formats_well() {
        let id = temp_identity();
        id.load().await.unwrap();
        id.set_preference("indent".into(), "4".into())
            .await
            .unwrap();
        id.add_dealbreaker("jQuery".into(), "deprecated".into(), vec!["jquery".into()])
            .await
            .unwrap();
        let ctx = id.inject_context().await;
        assert!(ctx.contains("indent"));
        assert!(ctx.contains("jQuery"));
    }
}
