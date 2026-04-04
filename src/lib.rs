#[cfg(feature = "search")]
pub mod embeddings;
pub mod format;
pub mod queries;
pub mod settings;

// Internal modules — used by main binary and lib internals, not by search binary
pub(crate) mod db;
pub(crate) mod engine;
pub(crate) mod path;
pub(crate) mod state;
pub(crate) mod util;
