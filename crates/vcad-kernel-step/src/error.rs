//! Error types for STEP file operations.

use thiserror::Error;

/// Errors that can occur during STEP file operations.
#[derive(Error, Debug)]
pub enum StepError {
    /// I/O error reading or writing a file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Parsing error from stepperoni.
    #[error(transparent)]
    Parse(#[from] stepperoni::StepError),

    /// Missing entity reference.
    #[error("Missing entity reference: #{0}")]
    MissingEntity(u64),

    /// Unsupported entity type.
    #[error("Unsupported entity type: {0}")]
    UnsupportedEntity(String),

    /// Invalid geometry (e.g., degenerate surface, invalid axis).
    #[error("Invalid geometry: {0}")]
    InvalidGeometry(String),

    /// Invalid topology (e.g., non-manifold edge, open shell).
    #[error("Invalid topology: {0}")]
    InvalidTopology(String),

    /// Type mismatch (e.g., expected CARTESIAN_POINT but got DIRECTION).
    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch {
        /// Expected type name.
        expected: String,
        /// Actual type name.
        actual: String,
    },

    /// Empty file or no solids found.
    #[error("No solids found in STEP file")]
    NoSolids,

    /// The read was cancelled through its [`crate::StepProgress`].
    #[error("cancelled")]
    Cancelled,
}

impl StepError {
    /// Create a parser-style error (wraps stepperoni's parser error).
    pub fn parser(entity_id: Option<u64>, message: impl Into<String>) -> Self {
        Self::Parse(stepperoni::StepError::parser(entity_id, message))
    }

    /// Create a type mismatch error.
    pub fn type_mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::TypeMismatch {
            expected: expected.into(),
            actual: actual.into(),
        }
    }
}
