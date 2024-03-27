use std::{io, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("No ELF file was provided. Specify the file path in the configuration file or command line arguments")]
    MissingElfFile,

    #[error("Failed to read the ELF file '{0}'")]
    ElfFileRead(PathBuf, #[source] io::Error),

    #[error("The ELF file does not contain a '.defmt' section")]
    MissingDefmtSection,

    #[error("Encountered an error while reading the defmt table from the ELF file. {0}")]
    DefmtTable(#[source] anyhow::Error),

    #[error("Encountered an error while reading the defmt location data from the ELF file. {0}")]
    DefmtLocation(#[source] anyhow::Error),

    #[error("Encountered a defmt parser error")]
    DefmtParser(#[from] defmt_parser::Error),

    #[error("Context manager is in an inconsistent state")]
    ContextManagerInternalState,

    #[error(
        "Encountered and IO error while reading the input channel ({})",
        .0.kind()
    )]
    Io(#[from] io::Error),

    #[error("Encountered an ingest client error. {0}")]
    Ingest(#[from] modality_ingest_client::IngestError),

    #[error("Encountered an ingest client error. {0}")]
    DynamicIngest(#[from] modality_ingest_client::dynamic::DynamicIngestError),

    #[error("Encountered an ingest client initialization error. {0}")]
    IngestClientInitialization(#[from] modality_ingest_client::IngestClientInitializationError),

    #[error("Failed to authenticate. {0}")]
    Auth(#[from] crate::config::AuthTokenError),

    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
}
