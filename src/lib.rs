pub mod embeddings;
pub mod format;
pub mod queries;

// Internal modules — used by main binary and lib internals, not by search binary
pub(crate) mod db;
pub(crate) mod engine;
pub(crate) mod path;
pub(crate) mod state;
pub(crate) mod util;
