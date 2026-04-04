// Modules exported for the search binary and shared types
#[cfg(feature = "search")]
pub mod embeddings;
pub mod format;
pub mod queries;
pub mod settings;
pub mod util;

// These modules are used by the main binary and by lib modules for shared types.
// Most functions are only called from the binary, hence allow(dead_code).
#[allow(dead_code)]
pub(crate) mod db;
#[allow(dead_code)]
pub(crate) mod engine;
#[allow(dead_code)]
pub(crate) mod path;
#[allow(dead_code)]
pub(crate) mod state;
