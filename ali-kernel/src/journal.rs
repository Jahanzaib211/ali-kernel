//! Journal — Hippocampus: episodic memory of sessions
//!
//! Nature psychology: The hippocampus stores episodic memories — what happened,
//! what mattered, the emotional context. Every session writes an entry. The journal
//! is the primary record of our shared history, not just a code-search index.
//!
//! Entries are dense, short, and retrieval-optimized. A background compactor
//! merges old entries into thematic threads.
//!
//! Storage: JSONL per session → SQLite index for fast lookup

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::Config;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Unique entry ID
    pub id: String,
    /// Wall clock when session started
    pub session_start: DateTime<Utc>,
    /// Wall clock when entry was written
    pub ts: DateTime<Utc>,
    /// Human-readable session name (e.g. "foo-project_bear")
    pub session_name: String,
    /// What the user was trying to accomplish (their intent in their words)
    pub intent_summary: String,
    /// What actually happened (concise narrative)
    pub what_happened: String,
    /// What was significant (decisions made, patterns discovered, frustrations)
    pub what_mattered: Vec<String>,
    /// Things that frustrated or blocked progress
    pub frustrations: Vec<String>,
    /// Wins — things that worked well
    pub wins: Vec<String>,
    /// Files read or modified
    pub files_touched: Vec<String>,
    /// Concepts encountered or clarified
    pub concepts: Vec<String>,
    /// What to pick up next session
    pub next_session_hooks: Vec<String>,
    /// Emotional tone at session end
    pub emotional_tone: EmotionalTone,
    /// User's energy level at session end
    pub energy_level: EnergyLevel,
    /// How long the session ran (seconds)
    pub duration_secs: u64,
    /// Number of tool calls executed
    pub tool_call_count: usize,
    /// User's explicit tags for this session
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmotionalTone {
    Curious,
    Focused,
    Frustrated,
    Satisfied,
    Excited,
    Tired,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnergyLevel {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub name: String,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub intent_summary: String,
    pub entry_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub metadata: SessionMetadata,
    /// Tool calls executed this session
    pub tool_calls: Vec<ToolCallRecord>,
    /// Key moments (intent stated, blocks hit, achievements)
    pub moments: Vec<SessionMoment>,
    /// Running frustration signals
    pub frustrations: Vec<String>,
    /// Running wins
    pub wins: Vec<String>,
    /// Files touched
    pub files_touched: Vec<String>,
    /// Concepts encountered
    pub concepts: Vec<String>,
    /// Energy noted by user
    pub energy: Option<EnergyLevel>,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub ts: DateTime<Utc>,
    pub success: bool,
    pub output_preview: String,
}

#[derive(Debug, Clone)]
pub enum SessionMoment {
    IntentStated { text: String },
    Blocked { tool: String, attempt: usize },
    Win { description: String },
    Frustration { description: String },
    Achieved { description: String },
}

// ── Journal Store ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Journal {
    store_path: PathBuf,
    sessions_path: PathBuf,
    /// In-progress session (None between sessions)
    active: Arc<RwLock<Option<ActiveSession>>>,
}

impl Journal {
    /// Open or create the journal at `~/.ali/journal/`
    pub fn new(config: &Config) -> Result<Self> {
        let journal_dir = config.ali_dir.join("journal");
        let store_path = journal_dir.join("entries");
        let sessions_path = journal_dir.join("sessions.jsonl");

        fs::create_dir_all(&store_path)?;

        Ok(Self {
            store_path,
            sessions_path,
            active: Arc::new(RwLock::new(None)),
        })
    }

    /// Begin a new session. Call at the start of each user interaction block.
    pub async fn begin_session(&self, session_name: String, intent_summary: String) {
        let meta = SessionMetadata {
            name: session_name.clone(),
            start: Utc::now(),
            end: None,
            intent_summary,
            entry_id: None,
        };
        let session = ActiveSession {
            metadata: meta,
            tool_calls: Vec::new(),
            moments: Vec::new(),
            frustrations: Vec::new(),
            wins: Vec::new(),
            files_touched: Vec::new(),
            concepts: Vec::new(),
            energy: None,
        };
        *self.active.write().await = Some(session);
        tracing::info!("JOURNAL session_begin name={}", session_name);
    }

    /// Record a tool call within the active session.
    pub async fn record_tool(&self, name: String, success: bool, output_preview: String) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            session.tool_calls.push(ToolCallRecord {
                name,
                ts: Utc::now(),
                success,
                output_preview,
            });
        }
    }

    /// Record a frustration signal within the active session.
    pub async fn record_frustration(&self, description: String) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            if !session.frustrations.contains(&description) {
                session.frustrations.push(description);
            }
        }
    }

    /// Record a win within the active session.
    pub async fn record_win(&self, description: String) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            if !session.wins.contains(&description) {
                session.wins.push(description);
            }
        }
    }

    /// Record a file touched within the active session.
    pub async fn record_file(&self, path: String) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            if !session.files_touched.contains(&path) {
                session.files_touched.push(path);
            }
        }
    }

    /// Record a concept encountered within the active session.
    pub async fn record_concept(&self, concept: String) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            if !session.concepts.contains(&concept) {
                session.concepts.push(concept);
            }
        }
    }

    /// Record a session moment.
    pub async fn record_moment(&self, moment: SessionMoment) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            session.moments.push(moment);
        }
    }

    /// Set energy level for the active session.
    pub async fn set_energy(&self, level: EnergyLevel) {
        let mut guard = self.active.write().await;
        if let Some(ref mut session) = *guard {
            session.energy = Some(level);
        }
    }

    /// Finalize and store the active session as a journal entry.
    /// Call at the end of a meaningful interaction block.
    pub async fn end_session(&self, emotional_tone: EmotionalTone) -> Option<JournalEntry> {
        // Take session out of the lock, release the guard
        let (session, end, id, entry) = {
            let mut guard = self.active.write().await;
            let session = guard.take()?;
            let end = Utc::now();
            let id = uuid::Uuid::new_v4().to_string();
            let duration = (end - session.metadata.start).num_seconds() as u64;
            let entry = JournalEntry {
                id: id.clone(),
                session_start: session.metadata.start,
                ts: end,
                session_name: session.metadata.name.clone(),
                intent_summary: session.metadata.intent_summary.clone(),
                what_happened: Self::summarize_session(&session),
                what_mattered: session.wins.clone(),
                frustrations: session.frustrations.clone(),
                wins: session.wins.clone(),
                files_touched: session.files_touched.clone(),
                concepts: session.concepts.clone(),
                next_session_hooks: Self::derive_hooks(&session),
                emotional_tone,
                energy_level: session.energy.unwrap_or(EnergyLevel::Medium),
                duration_secs: duration,
                tool_call_count: session.tool_calls.len(),
                tags: Vec::new(),
            };
            // guard dropped here, lock released
            (session, end, id, entry)
        };

        // Persist to JSONL (outside the lock)
        if let Err(e) = self.write_entry(&entry) {
            tracing::error!("JOURNAL write failed: {e}");
        }

        // Append session metadata
        let meta = SessionMetadata {
            name: session.metadata.name,
            start: session.metadata.start,
            end: Some(end),
            intent_summary: session.metadata.intent_summary,
            entry_id: Some(id.clone()),
        };
        if let Err(e) = self.append_session_meta(&meta, &id) {
            tracing::error!("JOURNAL session meta write failed: {e}");
        }

        tracing::info!(
            "JOURNAL session_end name={} tools={} duration={}s tone={:?}",
            entry.session_name,
            entry.tool_call_count,
            entry.duration_secs,
            emotional_tone
        );

        Some(entry)
    }

    /// Get the active session (read-only view). Returns None if no session is active.
    pub async fn get_active(&self) -> Option<ActiveSession> {
        self.active.read().await.clone()
    }

    /// Retrieve journal entries relevant to a query (keyword search across entries).
    pub async fn retrieve(&self, query: &str, limit: usize) -> Vec<JournalEntry> {
        let query_lower = query.to_lowercase();
        let query_tokens: Vec<&str> = query_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .collect();

        if query_tokens.is_empty() {
            return self.get_recent(limit).await;
        }

        let entries_dir = &self.store_path;
        let mut scored: Vec<(JournalEntry, f32)> = Vec::new();

        if let Ok(entries) = fs::read_dir(entries_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path) {
                    for line in content.lines().filter(|l| !l.is_empty()) {
                        if let Ok(e) = serde_json::from_str::<JournalEntry>(line) {
                            let score = Self::score_entry(&e, &query_tokens);
                            if score > 0.0 {
                                scored.push((e, score));
                            }
                        }
                    }
                }
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Less));
        scored.truncate(limit);
        scored.into_iter().map(|(e, _)| e).collect()
    }

    /// Get the N most recent journal entries.
    pub async fn get_recent(&self, limit: usize) -> Vec<JournalEntry> {
        let entries_dir = &self.store_path;
        let mut all_entries: Vec<JournalEntry> = Vec::new();

        if let Ok(entries) = fs::read_dir(entries_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Ok(content) = fs::read_to_string(&path) {
                    for line in content.lines().filter(|l| !l.is_empty()) {
                        if let Ok(e) = serde_json::from_str::<JournalEntry>(line) {
                            all_entries.push(e);
                        }
                    }
                }
            }
        }

        all_entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        all_entries.truncate(limit);
        all_entries
    }

    /// List all session names (for session browser / resume).
    pub async fn list_sessions(&self) -> Vec<SessionMetadata> {
        let mut sessions = Vec::new();
        if let Ok(content) = fs::read_to_string(&self.sessions_path) {
            for line in content.lines().filter(|l| !l.is_empty()) {
                if let Ok(meta) = serde_json::from_str::<SessionMetadata>(line) {
                    sessions.push(meta);
                }
            }
        }
        sessions.sort_by(|a, b| b.start.cmp(&a.start));
        sessions
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn write_entry(&self, entry: &JournalEntry) -> Result<()> {
        let filename = format!("{}_{}.jsonl", entry.session_start.format("%Y-%m-%d"), entry.session_name);
        let path = self.store_path.join(filename);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    fn append_session_meta(&self, meta: &SessionMetadata, entry_id: &str) -> Result<()> {
        let mut m = meta.clone();
        m.entry_id = Some(entry_id.to_string());
        let line = serde_json::to_string(&m)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.sessions_path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    fn score_entry(entry: &JournalEntry, query_tokens: &[&str]) -> f32 {
        let haystack = format!(
            "{} {} {} {:?} {:?}",
            entry.intent_summary,
            entry.what_happened,
            entry.files_touched.join(" "),
            entry.emotional_tone,
            entry.energy_level
        );
        let haystack_lower = haystack.to_lowercase();
        let hay_tokens: Vec<&str> = haystack_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .collect();

        if hay_tokens.is_empty() {
            return 0.0;
        }

        let overlap: usize = query_tokens
            .iter()
            .filter(|qt| hay_tokens.contains(qt))
            .count();

        let max_possible = query_tokens.len().min(hay_tokens.len());
        if max_possible == 0 {
            return 0.0;
        }

        // Recency boost — entries within 7 days get a 1.3x multiplier
        let days_old = (Utc::now() - entry.ts).num_days();
        let recency_boost = if days_old < 7 { 1.3 } else { 1.0 };

        (overlap as f32 / max_possible as f32) * recency_boost
    }

    fn summarize_session(session: &ActiveSession) -> String {
        let tool_count = session.tool_calls.len();
        let wins = session.wins.len();
        let frustrations = session.frustrations.len();
        let files = session.files_touched.len();
        format!(
            "{} tool calls, {} wins, {} frustrations, {} files touched",
            tool_count, wins, frustrations, files
        )
    }

    fn derive_hooks(session: &ActiveSession) -> Vec<String> {
        let mut hooks = Vec::new();
        // Incomplete frustrations are open threads
        for f in &session.frustrations {
            hooks.push(format!("Follow up: {}", f));
        }
        // Files that were opened but not resolved
        for f in &session.files_touched {
            hooks.push(format!("Continue work in: {}", f));
        }
        hooks.truncate(5);
        hooks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_journal() -> Journal {
        let cfg = Config {
            ali_dir: std::env::temp_dir().join("ali_journal_test"),
            ..Default::default()
        };
        Journal::new(&cfg).unwrap()
    }

    #[tokio::test]
    async fn begin_and_end_session() {
        let j = temp_journal();
        j.begin_session("test-session".into(), "test intent".into()).await;
        j.record_win("found a great pattern".into()).await;
        j.record_frustration("bubblewrap missing".into()).await;
        j.record_file("src/main.rs".into()).await;
        j.set_energy(EnergyLevel::High).await;
        let entry = j.end_session(EmotionalTone::Satisfied).await;
        assert!(entry.is_some());
        let e = entry.unwrap();
        assert_eq!(e.session_name, "test-session");
        assert_eq!(e.wins.len(), 1);
        assert_eq!(e.frustrations.len(), 1);
        assert_eq!(e.files_touched.len(), 1);
        assert_eq!(e.energy_level, EnergyLevel::High);
        assert_eq!(e.emotional_tone, EmotionalTone::Satisfied);
    }

    #[tokio::test]
    async fn retrieve_finds_recent_entry() {
        let j = temp_journal();
        j.begin_session("auth-refactor".into(), "JWT refresh tokens".into()).await;
        j.record_file("src/auth.rs".into()).await;
        j.end_session(EmotionalTone::Focused).await;
        let results = j.retrieve("JWT auth", 5).await;
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn get_recent_limits() {
        let j = temp_journal();
        for i in 0..12 {
            j.begin_session(format!("session-{}", i), format!("intent {}", i)).await;
            j.end_session(EmotionalTone::Neutral).await;
        }
        let recent = j.get_recent(5).await;
        assert_eq!(recent.len(), 5);
    }
}
