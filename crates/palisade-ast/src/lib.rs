//! `palisade-ast` — M0 stub. **Deliberately empty.**
//!
//! M2 lands the `tree-sitter-rust-orchard` wrapper here, with a content-hash
//! parse cache and a refusal to guess on parser error nodes (`slop-gate`'s
//! explicit choice).
//!
//! The crate exists in M0 so the boundary is established before anything wants
//! to cross it, and so that the CI process-boundary check has a crate to
//! recognise as "not allowed to spawn anything".

/// The parse-cache key scheme, fixed now so the fingerprint of a cached parse
/// is reproducible across versions: blake3 over the file's bytes, not its
/// path and not its mtime.
pub const PARSE_CACHE_KEY_SCHEME: &str = "blake3:content:v1";
