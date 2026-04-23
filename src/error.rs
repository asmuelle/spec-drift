use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SpecDriftError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse Rust source {path}: {source}")]
    RustParse {
        path: PathBuf,
        #[source]
        source: syn::Error,
    },

    #[error("project walk failed: {0}")]
    Walk(#[from] ignore::Error),

    #[error("invalid config at {path}: {message}")]
    Config { path: PathBuf, message: String },
}
