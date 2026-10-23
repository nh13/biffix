//! Make cargo recompile this crate whenever a migration file changes.
//!
//! `sqlx::migrate!` embeds `migrations/` at compile time, but cargo does not otherwise track that
//! directory as a build input — so a warm `target/` can keep an embedded migration set that is
//! stale relative to the files on disk (a newly-added migration silently not embedded). Declaring
//! the directory as a rerun trigger forces a rebuild — and thus a re-embed — when it changes.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
