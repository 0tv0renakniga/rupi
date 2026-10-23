use thiserror::Error;

/// Project-wide result type. Subsystems use [`AgentError`] so callers can
/// distinguish policy, verification, transport, and incomplete-feature errors.
pub type Result<T> = std::result::Result<T, AgentError>;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Jev error: {0}")]
    Jev(String),

    #[error("Wiki error: {0}")]
    Wiki(String),

    #[error("policy denied action: {0}")]
    PolicyDenied(String),

    #[error("verification blocked: {0}")]
    Blocked(String),

    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: crate::state_machine::LoopState,
        to: crate::state_machine::LoopState,
    },

    #[error("task cancelled")]
    Cancelled,

    #[error("{subsystem} is not implemented yet")]
    NotImplemented { subsystem: &'static str },
}

impl AgentError {
    /// Explicit placeholder for a subsystem that is intentionally deferred.
    ///
    /// Stubs must fail loudly rather than pretending to have completed work.
    pub fn not_implemented(subsystem: &'static str) -> Self {
        Self::NotImplemented { subsystem }
    }
}
