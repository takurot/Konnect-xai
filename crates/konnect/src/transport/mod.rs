pub mod http;
pub mod http_policy;
pub mod stdio;

use crate::config::{Config, TransportMode};
use http_policy::{HttpPolicy, HttpPolicyError};

/// The transports to start. An HTTP transport cannot be named without the
/// policy that was validated for it.
pub enum TransportPlan {
    Stdio,
    Http(HttpPolicy),
    Both(HttpPolicy),
}

impl TransportPlan {
    pub fn from_config(config: &Config) -> Result<Self, HttpPolicyError> {
        Ok(match config.transport {
            TransportMode::Stdio => Self::Stdio,
            TransportMode::Http => Self::Http(HttpPolicy::from_config(config)?),
            TransportMode::Both => Self::Both(HttpPolicy::from_config(config)?),
        })
    }
}
