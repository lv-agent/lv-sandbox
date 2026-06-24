use thiserror::Error;

#[derive(Debug, Error)]
pub enum LandlockError {
    #[error("Landlock unavailable: {0}")]
    Unavailable(String),

    #[error("ABI version requirement not met: required {required}, actual {actual}")]
    AbiTooLow { required: u32, actual: u32 },

    #[error("ruleset creation failed: {0}")]
    RulesetCreate(String),

    #[error("failed to add rule: {0}")]
    RuleAdd(String),

    #[error("restrict_self failed: {0}")]
    RestrictSelf(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
