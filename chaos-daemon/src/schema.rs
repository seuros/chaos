use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Identity {
    pub id: i64,
    pub name: String,
    pub persona: Option<String>,
    pub created_at: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Memory {
    pub id: i64,
    pub scope: String,
    pub category: String,
    pub content: String,
    pub confidence: f64,
    pub created_at: i64,
    pub accessed_at: i64,
    pub access_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Skill {
    pub name: String,
    pub definition: String,
    pub source: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: String,
    pub project: Option<String>,
    pub summary: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}
