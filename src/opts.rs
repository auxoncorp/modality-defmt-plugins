use clap::Parser;
use derive_more::Display;
use serde_with::DeserializeFromStr;
use std::{path::PathBuf, str::FromStr};
use url::Url;

#[derive(Parser, Debug, Clone, Default)]
pub struct ReflectorOpts {
    /// Use configuration from file
    #[clap(
        long = "config",
        name = "config file",
        env = "MODALITY_REFLECTOR_CONFIG",
        help_heading = "REFLECTOR CONFIGURATION"
    )]
    pub config_file: Option<PathBuf>,

    /// Modality auth token hex string used to authenticate with.
    /// Can also be provide via the MODALITY_AUTH_TOKEN environment variable.
    #[clap(
        long,
        name = "auth-token-hex-string",
        env = "MODALITY_AUTH_TOKEN",
        help_heading = "REFLECTOR CONFIGURATION"
    )]
    pub auth_token: Option<String>,

    /// The modalityd or modality-reflector ingest protocol parent service address
    ///
    /// The default value is `modality-ingest://127.0.0.1:14188`.
    ///
    /// You can talk directly to the default ingest server port with
    /// `--ingest-protocol-parent-url modality-ingest://127.0.0.1:14182`
    #[clap(
        long = "ingest-protocol-parent-url",
        name = "URL",
        help_heading = "REFLECTOR CONFIGURATION"
    )]
    pub protocol_parent_url: Option<Url>,

    /// Ingest client timeout
    #[clap(
        long,
        name = "client-timeout",
        help_heading = "REFLECTOR CONFIGURATION"
    )]
    pub client_timeout: Option<humantime::Duration>,

    /// Allow insecure TLS
    #[clap(
        short = 'k',
        long = "insecure",
        help_heading = "REFLECTOR CONFIGURATION"
    )]
    pub allow_insecure_tls: bool,

    /// Use the provided run ID instead of generating a random UUID
    #[clap(long, name = "run-id", help_heading = "REFLECTOR CONFIGURATION")]
    pub run_id: Option<String>,

    /// Use the provided clock ID instead of generating a random UUID
    #[clap(long, name = "clock-id", help_heading = "REFLECTOR CONFIGURATION")]
    pub clock_id: Option<String>,
}

#[derive(Parser, Debug, Clone, Default)]
pub struct DefmtOpts {
    /// Don't synthesize interactions between tasks and ISRs when a context switch occurs
    #[clap(long, help_heading = "DEFMT CONFIGURATION")]
    pub disable_interactions: bool,

    /// Use the provided init task name instead of the default ('main')
    #[clap(long, help_heading = "DEFMT CONFIGURATION")]
    pub init_task_name: Option<String>,

    /// The RTOS mode to use (none, rtic1)
    #[clap(long, name = "rtos-mode", help_heading = "DEFMT CONFIGURATION")]
    pub rtos_mode: Option<RtosMode>,
}

#[derive(
    Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default, Display, DeserializeFromStr,
)]
pub enum RtosMode {
    #[default]
    #[display(fmt = "none")]
    None,
    #[display(fmt = "rtic1")]
    Rtic1,
}

impl FromStr for RtosMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim().to_lowercase().as_ref() {
            "none" => RtosMode::None,
            "rtic1" => RtosMode::Rtic1,
            _ => return Err(format!("Unsupported RTOS mode '{s}'")),
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn rtos_mode() {
        assert_eq!(RtosMode::from_str("none"), Ok(RtosMode::None));
        assert_eq!(RtosMode::from_str("rtic1"), Ok(RtosMode::Rtic1));
        assert_eq!(
            RtosMode::from_str("rtic2"),
            Err("Unsupported RTOS mode 'rtic2'".to_owned())
        );
    }
}
