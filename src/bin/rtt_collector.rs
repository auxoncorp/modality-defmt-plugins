use clap::Parser;
use modality_defmt_plugin::{
    defmt_reader, tracing::try_init_tracing_subscriber, DefmtConfig, DefmtConfigEntry, DefmtOpts,
    Interruptor, ReflectorOpts,
};
use probe_rs::{
    config::MemoryRegion,
    probe::{list::Lister, DebugProbeSelector, WireProtocol},
    rtt::{Rtt, ScanRegion, UpChannel},
    Core, Permissions, Session, VectorCatchCondition,
};
use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::{debug, error};

/// Collect defmt data from an on-device RTT buffer
#[derive(Parser, Debug, Clone)]
#[clap(version)]
struct Opts {
    #[clap(flatten)]
    pub rf_opts: ReflectorOpts,

    #[clap(flatten)]
    pub defmt_opts: DefmtOpts,

    /// Specify a target attach timeout.
    /// When provided, the plugin will continually attempt to attach and search
    /// for a valid RTT control block anywhere in the target RAM.
    ///
    /// Accepts durations like "10ms" or "1minute 2seconds 22ms".
    #[clap(
        long,
        name = "attach-timeout",
        help_heading = "COLLECTOR CONFIGURATION"
    )]
    pub attach_timeout: Option<humantime::Duration>,

    /// Use the provided RTT control block address instead of scanning the target memory for it.
    #[clap(
        long,
        name = "control-block-address",
        help_heading = "COLLECTOR CONFIGURATION"
    )]
    pub control_block_address: Option<u32>,

    /// The RTT up (target to host) channel number to poll on (defaults to 0).
    #[clap(long, name = "up-channel", help_heading = "COLLECTOR CONFIGURATION")]
    pub up_channel: Option<usize>,

    /// Select a specific probe instead of opening the first available one.
    ///
    /// Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID.
    #[structopt(long = "probe", name = "probe", help_heading = "PROBE CONFIGURATION")]
    pub probe_selector: Option<DebugProbeSelector>,

    /// The target chip to attach to (e.g. STM32F407VE).
    #[clap(long, name = "chip", help_heading = "PROBE CONFIGURATION")]
    pub chip: Option<String>,

    /// Protocol used to connect to chip.
    /// Possible options: [swd, jtag].
    ///
    /// The default value is swd.
    #[structopt(long, name = "protocol", help_heading = "PROBE CONFIGURATION")]
    pub protocol: Option<WireProtocol>,

    /// The protocol speed in kHz.
    ///
    /// The default value is 4000.
    #[clap(long, name = "speed", help_heading = "PROBE CONFIGURATION")]
    pub speed: Option<u32>,

    /// The selected core to target.
    ///
    /// The default value is 0.
    #[clap(long, name = "core", help_heading = "PROBE CONFIGURATION")]
    pub core: Option<usize>,

    /// Reset the target on startup.
    #[clap(long, name = "reset", help_heading = "PROBE CONFIGURATION")]
    pub reset: bool,

    /// Chip description YAML file path.
    /// Provides custom target descriptions based on CMSIS Pack files.
    #[clap(
        long,
        name = "chip-description-path",
        help_heading = "PROBE CONFIGURATION"
    )]
    pub chip_description_path: Option<PathBuf>,

    /// The ELF file containing the defmt table and location information.
    #[clap(
        name = "elf-file",
        verbatim_doc_comment,
        help_heading = "DEFMT CONFIGURATION"
    )]
    pub elf_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    match do_main().await {
        Ok(()) => (),
        Err(e) => {
            eprintln!("{e}");
            let mut cause = e.source();
            while let Some(err) = cause {
                eprintln!("Caused by: {err}");
                cause = err.source();
            }
            std::process::exit(exitcode::SOFTWARE);
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Opts::parse();

    try_init_tracing_subscriber()?;

    let intr = Interruptor::new();
    let intr_clone = intr.clone();
    ctrlc::set_handler(move || {
        if intr_clone.is_set() {
            let exit_code = if cfg!(target_family = "unix") {
                // 128 (fatal error signal "n") + 2 (control-c is fatal error signal 2)
                130
            } else {
                // Windows code 3221225786
                // -1073741510 == C000013A
                -1073741510
            };
            std::process::exit(exit_code);
        }

        debug!("Shutdown signal received");
        intr_clone.set();
    })?;

    let mut defmt_cfg = DefmtConfig::load_merge_with_opts(
        DefmtConfigEntry::RttCollector,
        opts.rf_opts,
        opts.defmt_opts,
    )?;

    if let Some(elf_file) = opts.elf_file.as_ref() {
        defmt_cfg.plugin.elf_file = Some(elf_file.clone());
    }
    if let Some(to) = opts.attach_timeout {
        defmt_cfg.plugin.rtt_collector.attach_timeout = Some(to.into());
    }
    if let Some(addr) = opts.control_block_address {
        defmt_cfg.plugin.rtt_collector.control_block_address = addr.into();
    }
    if let Some(up_channel) = opts.up_channel {
        defmt_cfg.plugin.rtt_collector.up_channel = up_channel;
    }
    if let Some(ps) = &opts.probe_selector {
        defmt_cfg.plugin.rtt_collector.probe_selector = Some(ps.clone().into());
    }
    if let Some(c) = opts.chip {
        defmt_cfg.plugin.rtt_collector.chip = Some(c);
    }
    if let Some(p) = opts.protocol {
        defmt_cfg.plugin.rtt_collector.protocol = p;
    }
    if let Some(s) = opts.speed {
        defmt_cfg.plugin.rtt_collector.speed = s;
    }
    if let Some(c) = opts.core {
        defmt_cfg.plugin.rtt_collector.core = c;
    }
    if opts.reset {
        defmt_cfg.plugin.rtt_collector.reset = true;
    }
    if let Some(cd) = &opts.chip_description_path {
        defmt_cfg.plugin.rtt_collector.chip_description_path = Some(cd.clone());
    }

    let chip = defmt_cfg
        .plugin
        .rtt_collector
        .chip
        .clone()
        .ok_or(Error::MissingChip)?;

    if let Some(chip_desc) = &defmt_cfg.plugin.rtt_collector.chip_description_path {
        debug!(path = %chip_desc.display(), "Adding custom chip description");
        let f = fs::File::open(chip_desc)?;
        probe_rs::config::add_target_from_yaml(f)?;
    }

    let lister = Lister::new();
    let mut probe = if let Some(probe_selector) = &defmt_cfg.plugin.rtt_collector.probe_selector {
        debug!(probe_selector = %probe_selector.0, "Opening selected probe");
        lister.open(probe_selector.0.clone())?
    } else {
        let probes = lister.list_all();
        debug!(probes = probes.len(), "Opening first available probe");
        if probes.is_empty() {
            return Err(Error::NoProbesAvailable.into());
        }
        probes[0].open(&lister)?
    };

    debug!(protocol = %defmt_cfg.plugin.rtt_collector.protocol, speed = defmt_cfg.plugin.rtt_collector.speed, "Configuring probe");
    probe.select_protocol(defmt_cfg.plugin.rtt_collector.protocol)?;
    probe.set_speed(defmt_cfg.plugin.rtt_collector.speed)?;

    debug!(
        chip = chip,
        core = defmt_cfg.plugin.rtt_collector.core,
        "Attaching to chip"
    );

    let mut session = probe.attach(chip, Permissions::default())?;

    let rtt_scan_regions = session.target().rtt_scan_regions.clone();
    let mut rtt_scan_region = if rtt_scan_regions.is_empty() {
        ScanRegion::Ram
    } else {
        ScanRegion::Ranges(rtt_scan_regions)
    };
    if let Some(user_provided_addr) = defmt_cfg.plugin.rtt_collector.control_block_address {
        debug!(
            rtt_addr = user_provided_addr,
            "Using explicit RTT control block address"
        );
        rtt_scan_region = ScanRegion::Exact(user_provided_addr);
    } else if let Some(Ok(mut file)) = defmt_cfg.plugin.elf_file.as_ref().map(fs::File::open) {
        if let Some(rtt_addr) = get_rtt_symbol(&mut file) {
            debug!(rtt_addr = rtt_addr, "Found RTT symbol");
            rtt_scan_region = ScanRegion::Exact(rtt_addr as _);
        }
    }

    let memory_map = session.target().memory_map.clone();

    let mut core = session.core(defmt_cfg.plugin.rtt_collector.core)?;

    if defmt_cfg.plugin.rtt_collector.reset {
        debug!("Reset and halt core");
        core.reset_and_halt(Duration::from_millis(100))?;
    }

    // Disable any previous vector catching (i.e. user just ran probe-rs run or a debugger)
    core.disable_vector_catch(VectorCatchCondition::All)?;
    core.clear_all_hw_breakpoints()?;

    let mut rtt = match defmt_cfg.plugin.rtt_collector.attach_timeout {
        Some(to) if !to.0.is_zero() => {
            attach_retry_loop(&mut core, &memory_map, &rtt_scan_region, to.0)?
        }
        _ => {
            debug!("Attaching to RTT");
            Rtt::attach_region(&mut core, &memory_map, &rtt_scan_region)?
        }
    };

    if defmt_cfg.plugin.rtt_collector.reset {
        debug!("Run core");
        core.run()?;
    }

    let up_channel = rtt
        .up_channels()
        .take(defmt_cfg.plugin.rtt_collector.up_channel)
        .ok_or_else(|| Error::UpChannelInvalid(defmt_cfg.plugin.rtt_collector.up_channel))?;
    let up_channel_mode = up_channel.mode(&mut core)?;
    let up_channel_name = up_channel.name().unwrap_or("NA");
    debug!(channel = up_channel.number(), name = up_channel_name, mode = ?up_channel_mode, buffer_size = up_channel.buffer_size(), "Opened up channel");

    // TODO - add blocking/non-blocking mode control

    // Only hold onto the Core when we need to lock the debug probe driver (before each read/write)
    std::mem::drop(core);

    let session = Arc::new(Mutex::new(session));
    let session_clone = session.clone();
    let defmt_cfg_clone = defmt_cfg.clone();
    let mut join_handle: tokio::task::JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        let mut stream = DefmtRttReader::new(
            intr.clone(),
            session_clone,
            up_channel,
            defmt_cfg_clone.plugin.rtt_collector.core,
        );
        defmt_reader::run(&mut stream, defmt_cfg_clone, intr).await?;
        Ok(())
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            debug!("User signaled shutdown");
            // Wait for any on-going transfer to complete
            let _session = session.lock().unwrap();
            std::thread::sleep(Duration::from_millis(100));
            join_handle.abort();
        }
        res = &mut join_handle => {
            match res? {
                Ok(_) => {},
                Err(e) => {
                    error!(error = %e, "Encountered and error during streaming");
                    return Err(e.into())
                }
            }
        }
    };

    // TODO set channel mode to non-blocking on exit

    Ok(())
}

fn get_rtt_symbol<T: io::Read + io::Seek>(file: &mut T) -> Option<u64> {
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_ok() {
        if let Ok(binary) = goblin::elf::Elf::parse(buffer.as_slice()) {
            for sym in &binary.syms {
                if let Some(name) = binary.strtab.get_at(sym.st_name) {
                    if name == "_SEGGER_RTT" {
                        return Some(sym.st_value);
                    }
                }
            }
        }
    }
    None
}

fn attach_retry_loop(
    core: &mut Core,
    memory_map: &[MemoryRegion],
    scan_region: &ScanRegion,
    timeout: humantime::Duration,
) -> Result<Rtt, Error> {
    debug!(timeout = %timeout, "Attaching to RTT");
    let timeout: Duration = timeout.into();
    let start = Instant::now();
    while Instant::now().duration_since(start) <= timeout {
        match Rtt::attach_region(core, memory_map, scan_region) {
            Ok(rtt) => return Ok(rtt),
            Err(e) => {
                if matches!(e, probe_rs::rtt::Error::ControlBlockNotFound) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }

                return Err(e.into());
            }
        }
    }

    // Timeout reached
    Ok(Rtt::attach(core, memory_map)?)
}

#[derive(Debug, Error)]
enum Error {
    #[error("No probes available")]
    NoProbesAvailable,

    #[error(
        "Missing chip. Either supply it as a option at the CLI or a config file member 'chip'"
    )]
    MissingChip,

    #[error("The RTT up channel ({0}) is invalid")]
    UpChannelInvalid(usize),

    #[error("Encountered an error with the probe. {0}")]
    ProbeRs(#[from] probe_rs::Error),

    #[error("Encountered an error with the probe RTT instance. {0}")]
    ProbeRsRtt(#[from] probe_rs::rtt::Error),

    #[error(transparent)]
    DefmtReader(#[from] modality_defmt_plugin::Error),
}

struct DefmtRttReader {
    interruptor: Interruptor,
    session: Arc<Mutex<Session>>,
    ch: UpChannel,
    core_index: usize,
}

impl DefmtRttReader {
    pub fn new(
        interruptor: Interruptor,
        session: Arc<Mutex<Session>>,
        ch: UpChannel,
        core_index: usize,
    ) -> Self {
        Self {
            interruptor,
            session,
            ch,
            core_index,
        }
    }
}

impl io::Read for DefmtRttReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while !self.interruptor.is_set() {
            let rtt_bytes_read = {
                let mut session = self.session.lock().unwrap();
                let mut core = session
                    .core(self.core_index)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                self.ch
                    .read(&mut core, buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            };

            // NOTE: this is what probe-rs does
            //
            // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
            // Once we receive new data, we bump the frequency to 1kHz.
            //
            // If the polling frequency is too high, the USB connection to the probe
            // can become unstable. Hence we only pull as little as necessary.
            if rtt_bytes_read != 0 {
                std::thread::sleep(Duration::from_millis(1));
                return Ok(rtt_bytes_read);
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(0)
    }
}