//! What the parser must see, and what it must refuse to see.

use std::sync::Arc;

use palisade_ast::{ParseError, UnsafeKind, parse};

#[test]
fn public_functions_are_found_and_private_ones_are_not() {
    let src = r#"
pub fn open(a: i32) -> i32 { a }
fn hidden() {}
pub(crate) fn crate_only() {}
"#;
    let f = parse(src).expect("parses");
    let names: Vec<&str> = f.items().iter().map(|i| i.path.as_str()).collect();
    assert_eq!(names, vec!["open"], "got {names:?}");
}

#[test]
fn a_signature_excludes_the_body() {
    // A body change is not an API change. Including the body would make every
    // implementation edit a finding, and that is a gate nobody keeps.
    let a = parse("pub fn f() -> i32 { 1 }").expect("parses");
    let b = parse("pub fn f() -> i32 { 2 }").expect("parses");
    assert_eq!(a.items()[0].signature, b.items()[0].signature);

    let c = parse("pub fn f() -> u32 { 1 }").expect("parses");
    assert_ne!(a.items()[0].signature, c.items()[0].signature);
}

#[test]
fn reformatting_does_not_change_a_signature() {
    // rustfmt is not an API change. A gate that reports this gets switched off.
    let a = parse("pub fn f(a: i32, b: i32) -> i32 { a + b }").expect("parses");
    let b = parse("pub fn f(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a + b\n}").expect("parses");
    assert_eq!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn renaming_a_parameter_changes_the_signature() {
    // ...while a genuine signature change must not be normalised away.
    let a = parse("pub fn f(a: i32) -> i32 { a }").expect("parses");
    let b = parse("pub fn f(b: i32) -> i32 { b }").expect("parses");
    assert_ne!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn structs_enums_traits_and_aliases_are_found() {
    let src = r#"
pub struct S { pub x: i32 }
pub enum E { A, B }
pub trait T { fn m(&self); }
pub type Alias = S;
pub const C: i32 = 1;
pub static ST: i32 = 2;
"#;
    let f = parse(src).expect("parses");
    let mut names: Vec<&str> = f.items().iter().map(|i| i.path.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["Alias", "C", "E", "S", "ST", "T"],
        "got {names:?}"
    );
}

#[test]
fn a_trait_impl_is_recorded_as_a_trait_impl() {
    // This is one of the four signature changes the M2 exit criterion names.
    let f = parse("pub struct S;\nimpl T for S { fn m(&self) {} }").expect("parses");
    assert!(
        f.items().iter().any(|i| i.kind.as_str() == "trait impl"),
        "got {:?}",
        f.items()
    );
}

#[test]
fn a_widened_generic_bound_changes_the_signature() {
    let a = parse("pub fn f<T: Clone>(t: T) {}").expect("parses");
    let b = parse("pub fn f<T: Clone + Send>(t: T) {}").expect("parses");
    assert_ne!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn every_unsafe_site_is_classified() {
    let src = r#"
pub unsafe fn danger() {}
pub struct S;
unsafe impl Send for S {}
pub fn uses() { unsafe { let _p = 1; } }
"#;
    let f = parse(src).expect("parses");
    let kinds: Vec<UnsafeKind> = f.unsafe_sites().iter().map(|u| u.kind).collect();
    for want in [UnsafeKind::Function, UnsafeKind::Impl, UnsafeKind::Block] {
        assert!(kinds.contains(&want), "missing {want:?} in {kinds:?}");
    }
}

#[test]
fn suppressions_are_extracted_with_their_lints() {
    let f = parse("#[allow(clippy::all)]\npub fn f() {}").expect("parses");
    assert_eq!(f.suppressions().len(), 1);
    let s = &f.suppressions()[0];
    assert_eq!(s.attribute, "allow");
    assert_eq!(s.lints, vec!["clippy::all"]);
    assert!(s.is_wildcard());
    assert_eq!(s.breadth(), 2);
}

#[test]
fn a_derive_is_not_a_suppression() {
    // Otherwise this gate fires on every derive in the codebase.
    let f = parse("#[derive(Debug, Clone)]\npub struct S;").expect("parses");
    assert!(f.suppressions().is_empty(), "got {:?}", f.suppressions());
}

#[test]
fn a_narrow_suppression_has_less_breadth_than_a_wildcard() {
    let narrow = parse("#[allow(clippy::needless_range_loop)]\npub fn f() {}").expect("parses");
    let wide = parse("#[allow(clippy::all)]\npub fn f() {}").expect("parses");
    assert!(narrow.suppressions()[0].breadth() < wide.suppressions()[0].breadth());
}

#[test]
fn a_file_that_does_not_parse_is_refused() {
    // `slop-gate` rejects files with parser error nodes rather than
    // recovering, and so does this. A file that does not parse has no
    // dependable answer, so the gate must say it could not tell.
    let err = parse("pub fn broken( {").unwrap_err();
    assert!(matches!(err, ParseError::ParseErrors { .. }), "got {err:?}");
    assert!(err.to_string().contains("could not tell"), "{err}");
}

#[test]
fn a_file_that_is_not_rust_at_all_is_refused() {
    let err = parse("this is a README, not Rust\n\n[[[").unwrap_err();
    assert!(matches!(err, ParseError::ParseErrors { .. }), "got {err:?}");
}

#[test]
fn the_cache_is_keyed_on_content_not_path() {
    // A path-keyed cache goes stale on a rewrite; an mtime-keyed one goes
    // stale on a checkout that preserves timestamps. Either would return the
    // previous file's answer, which is worse than re-parsing.
    let cache = palisade_ast::ParseCache::new();
    let a = cache.parse("pub fn f() {}");
    let b = cache.parse("pub fn f() {}");
    assert!(Arc::ptr_eq(&a, &b), "identical content must hit the cache");

    let c = cache.parse("pub fn g() {}");
    assert!(!Arc::ptr_eq(&a, &c), "different content must miss");
    assert_eq!(cache.len(), 2);
}

// ---- doc comments are not part of a signature -------------------------------
//
// Found by the M5 corpus on `pearls`: 7 of 8 "signature changed" findings
// differed only in documentation prose, with a reader unable to tell which
// change was real. A doc comment is not the API, and a gate that reports
// prose edits as a signature change is a gate whose real findings get skimmed.

#[test]
fn a_doc_comment_change_is_not_a_signature_change() {
    let a = parse("/// Creates a pearl.\n/// * `priority` - 0-4\npub fn f(priority: u8) {}")
        .expect("parses");
    let b = parse(
        "/// Creates a pearl.\n/// * `priority` - optional priority\npub fn f(priority: u8) {}",
    )
    .expect("parses");
    assert_eq!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn a_doc_comment_on_a_struct_is_not_a_signature_change() {
    let a = parse("/// A pearl.\n/// With a title.\npub struct P { pub title: String }")
        .expect("parses");
    let b = parse("/// A pearl.\npub struct P { pub title: String }").expect("parses");
    assert_eq!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn a_real_signature_change_still_shows_through_doc_comments() {
    // The corpus check cuts both ways. A `where` clause added alongside a
    // rewritten comment is still an API change.
    let a = parse("/// Old.\npub fn f<T>(t: T) where T: Clone {}").expect("parses");
    let b =
        parse("/// New.\npub fn f<T, E>(t: T) -> Result<T, E> where T: Clone, E: From<Error> {}")
            .expect("parses");
    assert_ne!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn a_block_doc_comment_is_stripped() {
    let a = parse("/** A pearl. */\npub struct P;").expect("parses");
    let b = parse("pub struct P;").expect("parses");
    assert_eq!(a.items()[0].signature, b.items()[0].signature);
}

#[test]
fn an_inner_doc_comment_between_items_is_stripped() {
    // `//!` between items attaches to the following one, and must not be read
    // as part of the previous item's signature.
    let src = "pub mod a {}\n//! inner\npub mod b {}\n";
    let f = parse(src).expect("parses");
    assert!(
        f.items().iter().all(|i| !i.signature.contains("inner")),
        "{:?}",
        f.items()
    );
}
