use clap::Parser;
use clap_stdin::{FileOrStdin, Source};
use modality_defmt_plugin::{
    defmt_reader, tracing::try_init_tracing_subscriber, DefmtConfig, DefmtConfigEntry, DefmtOpts,
    Interruptor, ReflectorOpts,
};
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tracing::{debug, error};

/// Import defmt data from a file or stdin
#[derive(Parser, Debug, Clone)]
#[clap(version)]
pub struct Opts {
    #[clap(flatten)]
    pub rf_opts: ReflectorOpts,

    #[clap(flatten)]
    pub defmt_opts: DefmtOpts,

    /// The ELF file containing the defmt table and location information.
    #[clap(
        long,
        name = "elf-file",
        verbatim_doc_comment,
        help_heading = "DEFMT CONFIGURATION"
    )]
    pub elf_file: Option<PathBuf>,

    /// Specify an open device/file timeout.
    /// When provided, the plugin will continually attempt to open the input.
    ///
    /// Accepts durations like "10ms" or "1minute 2seconds 22ms".
    #[clap(long, name = "open-timeout", help_heading = "COLLECTOR CONFIGURATION")]
    pub open_timeout: Option<humantime::Duration>,

    /// Input file or stdin stream to read from ('-' for stdin)
    #[clap(name = "input", help_heading = "IMPORTER CONFIGURATION")]
    pub input: Option<FileOrStdin>,
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
    let intr_clone: Interruptor = intr.clone();
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
        } else {
            intr_clone.set();
        }
    })?;

    let mut defmt_cfg = DefmtConfig::load_merge_with_opts(
        DefmtConfigEntry::Importer,
        opts.rf_opts,
        opts.defmt_opts,
    )?;

    if let Some(elf_file) = opts.elf_file.as_ref() {
        defmt_cfg.plugin.elf_file = Some(elf_file.clone());
    }
    if let Some(to) = opts.open_timeout {
        defmt_cfg.plugin.import.open_timeout = Some(to.into());
    }

    enum Input {
        Stdin,
        File(File),
    }

    let input = if let Some(cli_input) = opts.input {
        debug!(source = ?cli_input.source, "Reading from input");
        match cli_input.source {
            Source::Stdin => Input::Stdin,
            Source::Arg(f) => Input::File(match defmt_cfg.plugin.import.open_timeout {
                Some(to) if !to.0.is_zero() => open_retry_loop(f, to.0)?,
                _ => File::open(&f).map_err(|_| FileOpenError(f.into()))?,
            }),
        }
    } else if let Some(input_file) = &defmt_cfg.plugin.import.file {
        debug!(source = %input_file.display(), "Reading from input");
        let input = match defmt_cfg.plugin.import.open_timeout {
            Some(to) if !to.0.is_zero() => open_retry_loop(input_file, to.0)?,
            _ => File::open(input_file).map_err(|_| FileOpenError(input_file.into()))?,
        };
        Input::File(input)
    } else {
        return Err("Missing import file or input stream. Either supply it as a positional argument at the CLI or in a config file".into());
    };

    let mut join_handle = tokio::spawn(async move {
        match input {
            Input::Stdin => {
                let mut r = std::io::stdin();
                defmt_reader::run(&mut r, defmt_cfg, intr).await
            }
            Input::File(f) => {
                let mut r = BufReader::new(f);
                defmt_reader::run(&mut r, defmt_cfg, intr).await
            }
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            debug!("User signaled shutdown");
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

    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to open input file '{0:?}'")]
struct FileOpenError(PathBuf);

fn open_retry_loop<P: AsRef<Path>>(
    p: P,
    timeout: humantime::Duration,
) -> Result<File, FileOpenError> {
    debug!(timeout = %timeout, "Starting input open retry loop");
    let timeout: Duration = timeout.into();
    let start = Instant::now();
    while Instant::now().duration_since(start) <= timeout {
        match File::open(p.as_ref()) {
            Ok(f) => return Ok(f),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        }
    }

    // Timeout reached
    File::open(p.as_ref()).map_err(|_| FileOpenError(p.as_ref().into()))
}
