use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Memory {
    pub content: String,
    pub score: f32,
    pub source: String,
}

pub async fn retrieve(_query: &str, _config: &Config) -> Vec<Memory> {
    // Phase 3: Qdrant vector store integration
    vec![]
}
