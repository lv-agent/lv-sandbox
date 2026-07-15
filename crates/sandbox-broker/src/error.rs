use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("sandbox http error: {0}")]
    Http(String),
    #[error("sandbox returned non-2xx: {status} {body}")]
    Status { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(String),
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("missing field '{0}' in tool input")]
    MissingField(&'static str),
    #[error("tool failed: {0}")]
    Tool(String),
}

impl From<reqwest::Error> for BridgeError {
    fn from(e: reqwest::Error) -> Self {
        BridgeError::Http(e.to_string())
    }
}
