use crate::{
    opts::{DefmtOpts, ReflectorOpts, RtosMode},
    time::Rate,
};
use auxon_sdk::{
    auth_token::AuthToken,
    reflector_config::{Config, TomlValue, TopLevelIngest, CONFIG_ENV_VAR},
};
use derive_more::{Deref, From, Into};
use serde::Deserialize;
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum AuthTokenError {
    #[error(transparent)]
    StringDeserialization(#[from] auxon_sdk::auth_token::AuthTokenStringDeserializationError),

    #[error(transparent)]
    LoadAuthTokenError(#[from] auxon_sdk::auth_token::LoadAuthTokenError),
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub enum DefmtConfigEntry {
    #[default]
    Importer,
    RttCollector,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DefmtConfig {
    pub auth_token: Option<String>,
    pub ingest: TopLevelIngest,
    pub plugin: PluginConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PluginConfig {
    pub client_timeout: Option<HumanTime>,
    pub run_id: Option<String>,
    pub clock_id: Option<String>,
    pub init_task_name: Option<String>,
    pub disable_interactions: bool,
    pub clock_rate: Option<Rate>,
    pub rtos_mode: RtosMode,
    pub elf_file: Option<PathBuf>,

    pub import: ImportConfig,
    pub rtt_collector: RttCollectorConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ImportConfig {
    pub open_timeout: Option<HumanTime>,
    pub file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct RttCollectorConfig {
    pub attach_timeout: Option<HumanTime>,
    pub control_block_address: Option<u32>,
    pub up_channel: usize,
    pub probe_selector: Option<ProbeSelector>,
    pub chip: Option<String>,
    pub protocol: probe_rs::probe::WireProtocol,
    pub speed: u32,
    pub core: usize,
    pub reset: bool,
    pub attach_under_reset: bool,
    pub chip_description_path: Option<PathBuf>,
    pub thumb: bool,
    pub setup_on_breakpoint: Option<String>,
    pub rtt_read_buffer_size: usize,
    pub rtt_poll_interval: Option<HumanTime>,
    pub metrics: bool,
}

impl RttCollectorConfig {
    pub const DEFAULT_UP_CHANNEL: usize = 0;
    pub const DEFAULT_PROTOCOL: probe_rs::probe::WireProtocol = probe_rs::probe::WireProtocol::Swd;
    pub const DEFAULT_SPEED: u32 = 4000;
    pub const DEFAULT_CORE: usize = 0;
    const DEFAULT_RTT_BUFFER_SIZE: usize = 1024;
}

impl Default for RttCollectorConfig {
    fn default() -> Self {
        Self {
            attach_timeout: None,
            control_block_address: None,
            up_channel: Self::DEFAULT_UP_CHANNEL,
            probe_selector: None,
            chip: None,
            protocol: Self::DEFAULT_PROTOCOL,
            speed: Self::DEFAULT_SPEED,
            core: Self::DEFAULT_CORE,
            reset: false,
            attach_under_reset: false,
            chip_description_path: None,
            thumb: false,
            setup_on_breakpoint: None,
            rtt_read_buffer_size: Self::DEFAULT_RTT_BUFFER_SIZE,
            rtt_poll_interval: None,
            metrics: false,
        }
    }
}

#[derive(Clone, Debug, From, Into, Deref, serde_with::DeserializeFromStr)]
pub struct ProbeSelector(pub probe_rs::probe::DebugProbeSelector);

impl PartialEq for ProbeSelector {
    fn eq(&self, other: &Self) -> bool {
        self.0.vendor_id == other.0.vendor_id
            && self.0.product_id == other.0.product_id
            && self.0.serial_number == other.0.serial_number
    }
}

impl Eq for ProbeSelector {}

impl FromStr for ProbeSelector {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(
            probe_rs::probe::DebugProbeSelector::from_str(s).map_err(|e| e.to_string())?,
        ))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, From, Into, Deref, serde_with::DeserializeFromStr)]
pub struct HumanTime(pub humantime::Duration);

impl FromStr for HumanTime {
    type Err = humantime::DurationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(humantime::Duration::from_str(s)?))
    }
}

impl DefmtConfig {
    pub fn load_merge_with_opts(
        entry: DefmtConfigEntry,
        rf_opts: ReflectorOpts,
        defmt_opts: DefmtOpts,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cfg = if let Some(cfg_path) = &rf_opts.config_file {
            auxon_sdk::reflector_config::try_from_file(cfg_path)?
        } else if let Ok(env_path) = env::var(CONFIG_ENV_VAR) {
            auxon_sdk::reflector_config::try_from_file(Path::new(&env_path))?
        } else {
            Config::default()
        };

        let mut ingest = cfg.ingest.clone().unwrap_or_default();
        if let Some(url) = &rf_opts.protocol_parent_url {
            ingest.protocol_parent_url = Some(url.clone());
        }
        if rf_opts.allow_insecure_tls {
            ingest.allow_insecure_tls = true;
        }

        let cfg_plugin = PluginConfig::from_metadata(&cfg, entry)?;
        let plugin = PluginConfig {
            client_timeout: rf_opts
                .client_timeout
                .map(|t| t.into())
                .or(cfg_plugin.client_timeout),
            run_id: rf_opts.run_id.or(cfg_plugin.run_id),
            clock_id: rf_opts.clock_id.or(cfg_plugin.clock_id),
            init_task_name: defmt_opts.init_task_name.or(cfg_plugin.init_task_name),
            disable_interactions: if defmt_opts.disable_interactions {
                true
            } else {
                cfg_plugin.disable_interactions
            },
            clock_rate: defmt_opts.clock_rate.or(cfg_plugin.clock_rate),
            rtos_mode: defmt_opts.rtos_mode.unwrap_or(cfg_plugin.rtos_mode),
            elf_file: cfg_plugin.elf_file, // NOTE: plugin opts handling may override this
            import: cfg_plugin.import,
            rtt_collector: cfg_plugin.rtt_collector,
        };

        Ok(Self {
            auth_token: rf_opts.auth_token,
            ingest,
            plugin,
        })
    }

    pub fn protocol_parent_url(&self) -> Result<Url, url::ParseError> {
        if let Some(url) = &self.ingest.protocol_parent_url {
            Ok(url.clone())
        } else {
            let url = Url::parse("modality-ingest://127.0.0.1:14188")?;
            Ok(url)
        }
    }

    pub fn resolve_auth(&self) -> Result<AuthToken, AuthTokenError> {
        if let Some(auth_token_hex) = self.auth_token.as_deref() {
            Ok(auxon_sdk::auth_token::decode_auth_token_hex(
                auth_token_hex,
            )?)
        } else {
            Ok(AuthToken::load()?)
        }
    }
}

mod internal {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
    #[serde(rename_all = "kebab-case", default)]
    pub struct CommonPluginConfig {
        pub client_timeout: Option<HumanTime>,
        pub run_id: Option<String>,
        pub clock_id: Option<String>,
        pub init_task_name: Option<String>,
        pub disable_interactions: bool,
        pub clock_rate: Option<Rate>,
        pub rtos_mode: RtosMode,
        pub elf_file: Option<PathBuf>,
    }

    impl From<CommonPluginConfig> for PluginConfig {
        fn from(c: CommonPluginConfig) -> Self {
            Self {
                client_timeout: c.client_timeout,
                run_id: c.run_id,
                clock_id: c.clock_id,
                init_task_name: c.init_task_name,
                disable_interactions: c.disable_interactions,
                clock_rate: c.clock_rate,
                rtos_mode: c.rtos_mode,
                elf_file: c.elf_file,
                import: Default::default(),
                rtt_collector: Default::default(),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
    #[serde(rename_all = "kebab-case", default)]
    pub struct ImportPluginConfig {
        #[serde(flatten)]
        pub common: CommonPluginConfig,
        #[serde(flatten)]
        pub import: ImportConfig,
    }

    impl From<ImportPluginConfig> for PluginConfig {
        fn from(pc: ImportPluginConfig) -> Self {
            let ImportPluginConfig { common, import } = pc;
            let mut c = PluginConfig::from(common);
            c.import = import;
            c
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
    #[serde(rename_all = "kebab-case", default)]
    pub struct RttCollectorPluginConfig {
        #[serde(flatten)]
        pub common: CommonPluginConfig,
        #[serde(flatten)]
        pub rtt_collector: RttCollectorConfig,
    }

    impl From<RttCollectorPluginConfig> for PluginConfig {
        fn from(pc: RttCollectorPluginConfig) -> Self {
            let RttCollectorPluginConfig {
                common,
                rtt_collector,
            } = pc;
            let mut c = PluginConfig::from(common);
            c.rtt_collector = rtt_collector;
            c
        }
    }
}

impl PluginConfig {
    pub(crate) fn from_metadata(
        cfg: &Config,
        entry: DefmtConfigEntry,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use internal::{ImportPluginConfig, RttCollectorPluginConfig};
        match entry {
            DefmtConfigEntry::Importer => {
                Self::from_cfg_metadata::<ImportPluginConfig>(cfg).map(|c| c.into())
            }
            DefmtConfigEntry::RttCollector => {
                Self::from_cfg_metadata::<RttCollectorPluginConfig>(cfg).map(|c| c.into())
            }
        }
    }

    fn from_cfg_metadata<'a, T: Deserialize<'a>>(
        cfg: &Config,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let cfg = TomlValue::Table(cfg.metadata.clone().into_iter().collect()).try_into()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use auxon_sdk::reflector_config::{AttrKeyEqValuePair, TimelineAttributes};
    use pretty_assertions::assert_eq;
    use std::{env, fs::File, io::Write};

    const IMPORTER_CONFIG: &str = r#"[ingest]
protocol-parent-url = 'modality-ingest://127.0.0.1:14182'
additional-timeline-attributes = [
    "ci_run=1",
    "platform='RTICv1'",
    "defmt-mode='rtt'",
]

[metadata]
client-timeout = "1s"
run-id = 'a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3'
clock-id = 'a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3'
init-task-name = 'main'
disable-interactions = true
rtos-mode = "rtic1"
clock-rate = "1/1000000"
elf-file = "fw.elf"
open-timeout = "100ms"
file = "rtt_log.bin"
"#;

    const RTT_COLLECTOR_CONFIG: &str = r#"[ingest]
protocol-parent-url = 'modality-ingest://127.0.0.1:14182'
additional-timeline-attributes = [
    "ci_run=1",
    "platform='RTICv1'",
    "module='m3'",
    "defmt-mode='rtt'",
]

[metadata]
client-timeout = "1s"
run-id = 'a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3'
clock-id = 'a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3'
init-task-name = 'fw'
disable-interactions = true
rtos-mode = "rtic1"
elf-file = "fw.elf"
clock-rate = "1/2000000"
attach-timeout = "100ms"
up-channel = 1
control-block-address = 0xFFFFF
down-channel = 1
probe-selector = '234:234'
chip = 'stm32'
protocol = 'Jtag'
speed = 1234
core = 1
reset = true
attach-under-reset = true
chip-description-path = "/tmp/stm32.yaml"
thumb = true
setup-on-breakpoint = "main"
rtt-poll-interval = "1ms"
rtt-read-buffer-size = 1024
metrics = true
"#;

    // Do a basic round trip check while we're at it
    fn get_cfg(content: &str, entry: DefmtConfigEntry) -> DefmtConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my_config.toml");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(content.as_bytes()).unwrap();
            f.flush().unwrap();
        }

        let cfg = DefmtConfig::load_merge_with_opts(
            entry,
            ReflectorOpts {
                config_file: Some(path.to_path_buf()),
                ..Default::default()
            },
            Default::default(),
        )
        .unwrap();

        env::set_var(CONFIG_ENV_VAR, path);
        let env_cfg =
            DefmtConfig::load_merge_with_opts(entry, Default::default(), Default::default())
                .unwrap();
        env::remove_var(CONFIG_ENV_VAR);
        assert_eq!(cfg, env_cfg);
        cfg
    }

    #[test]
    fn importer_cfg() {
        let cfg = get_cfg(IMPORTER_CONFIG, DefmtConfigEntry::Importer);
        assert_eq!(
            cfg,
            DefmtConfig {
                auth_token: None,
                ingest: TopLevelIngest {
                    protocol_parent_url: Url::parse("modality-ingest://127.0.0.1:14182")
                        .unwrap()
                        .into(),
                    allow_insecure_tls: false,
                    protocol_child_port: None,
                    timeline_attributes: TimelineAttributes {
                        additional_timeline_attributes: vec![
                            AttrKeyEqValuePair::from_str("ci_run=1").unwrap(),
                            AttrKeyEqValuePair::from_str("platform='RTICv1'").unwrap(),
                            AttrKeyEqValuePair::from_str("defmt-mode='rtt'").unwrap(),
                        ],
                        override_timeline_attributes: Default::default(),
                    },
                    max_write_batch_staleness: None,
                },
                plugin: PluginConfig {
                    client_timeout: HumanTime::from_str("1s").unwrap().into(),
                    run_id: "a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3".to_string().into(),
                    clock_id: "a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3".to_owned().into(),
                    init_task_name: "main".to_owned().into(),
                    disable_interactions: true,
                    rtos_mode: RtosMode::Rtic1,
                    clock_rate: Some(Rate::new(1, 1000000).unwrap()),
                    elf_file: PathBuf::from("fw.elf").into(),
                    import: ImportConfig {
                        open_timeout: HumanTime::from_str("100ms").unwrap().into(),
                        file: PathBuf::from("rtt_log.bin").into(),
                    },
                    rtt_collector: Default::default(),
                },
            }
        );
    }

    #[test]
    fn rtt_collector_cfg() {
        let cfg = get_cfg(RTT_COLLECTOR_CONFIG, DefmtConfigEntry::RttCollector);
        assert_eq!(
            cfg,
            DefmtConfig {
                auth_token: None,
                ingest: TopLevelIngest {
                    protocol_parent_url: Url::parse("modality-ingest://127.0.0.1:14182")
                        .unwrap()
                        .into(),
                    allow_insecure_tls: false,
                    protocol_child_port: None,
                    timeline_attributes: TimelineAttributes {
                        additional_timeline_attributes: vec![
                            AttrKeyEqValuePair::from_str("ci_run=1").unwrap(),
                            AttrKeyEqValuePair::from_str("platform='RTICv1'").unwrap(),
                            AttrKeyEqValuePair::from_str("module='m3'").unwrap(),
                            AttrKeyEqValuePair::from_str("defmt-mode='rtt'").unwrap(),
                        ],
                        override_timeline_attributes: Default::default(),
                    },
                    max_write_batch_staleness: None,
                },
                plugin: PluginConfig {
                    client_timeout: HumanTime::from_str("1s").unwrap().into(),
                    run_id: "a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3".to_string().into(),
                    clock_id: "a1a2a3a4b1b2c1c2d1d2d3d4d5d6d7d3".to_owned().into(),
                    init_task_name: "fw".to_owned().into(),
                    disable_interactions: true,
                    rtos_mode: RtosMode::Rtic1,
                    clock_rate: Some(Rate::new(1, 2000000).unwrap()),
                    elf_file: PathBuf::from("fw.elf").into(),
                    import: Default::default(),
                    rtt_collector: RttCollectorConfig {
                        attach_timeout: HumanTime::from_str("100ms").unwrap().into(),
                        control_block_address: 0xFFFFF_u32.into(),
                        up_channel: 1,
                        probe_selector: ProbeSelector::from_str("234:234").unwrap().into(),
                        chip: "stm32".to_owned().into(),
                        protocol: probe_rs::probe::WireProtocol::Jtag,
                        speed: 1234,
                        core: 1,
                        reset: true,
                        attach_under_reset: true,
                        chip_description_path: PathBuf::from("/tmp/stm32.yaml").into(),
                        thumb: true,
                        setup_on_breakpoint: Some("main".to_owned()),
                        rtt_poll_interval: HumanTime::from_str("1ms").unwrap().into(),
                        rtt_read_buffer_size: 1024,
                        metrics: true,
                    },
                },
            }
        );
    }
}
