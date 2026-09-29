//! `palisade-ast` — the tree-sitter wrapper, and the one place that decides
//! whether a file can be reasoned about.
//!
//! Two decisions here are load-bearing.
//!
//! **Refuse to guess on parser error nodes.** `slop-gate` states the same
//! boundary and the same reason: it rejects files with error nodes rather than
//! applying a recovery workaround. A file that does not parse has no
//! dependable answer, so a gate reading it must return `Untrustworthy` — not a
//! guess, and not silence. M1's `is_attr` bug is the cautionary tale for the
//! alternative: a matcher that quietly matches nothing produces a gate that
//! looks calibrated and is not.
//!
//! **Cache by content, not by path or mtime.** The cache key is blake3 over
//! the file's bytes. A path-keyed cache goes stale on a rewrite, and an
//! mtime-keyed one goes stale on a checkout that preserves timestamps. Both
//! would return a previous file's answer, which is worse than re-parsing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The parse-cache key scheme. Named in a constant so a cache written by one
/// version is not silently read by another.
pub const PARSE_CACHE_KEY_SCHEME: &str = "blake3:content:v1";

/// Why a file could not be parsed well enough to reason about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The grammar could not be loaded. A build problem, not a source problem.
    GrammarUnavailable(String),
    /// The source contains error or missing nodes.
    ///
    /// The count is included because "one syntax error" and "this is not Rust"
    /// are different situations and deserve different messages.
    ParseErrors {
        /// How many error or missing nodes were found.
        count: usize,
        /// The first one, 1-based line.
        first_line: Option<usize>,
    },
    /// A declaration's text came out empty, so its signature cannot be
    /// compared against anything.
    ///
    /// This exists because an empty signature is not a neutral value — it reads
    /// as "unchanged", which is the one thing a two-tree comparison must never
    /// conclude by accident. It happened: `strip_doc_comments` matched `//`
    /// inside `///` and then hunted for a block-comment close in a line
    /// comment, swallowing every documented declaration in the file, and
    /// `public_api_unchanged` silently reported no change for all of them.
    ///
    /// Refusing the parse is stronger than a later assertion. An assertion can
    /// be skipped, disabled, or removed under pressure; an unconstructible
    /// value cannot be. It also keeps the failure *loud* rather than
    /// *plausible*, which is the whole lesson: three times now, a correct-looking
    /// empty or partial value has produced a confidently wrong answer, and every
    /// one was caught by running rather than by reading.
    DegenerateSignature {
        /// The kind of declaration it was, e.g. `function_item`.
        kind: &'static str,
        /// Its name, when it has one.
        name: String,
        /// 1-based line.
        line: u32,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GrammarUnavailable(e) => write!(f, "Rust grammar unavailable: {e}"),
            Self::DegenerateSignature { kind, name, line } => write!(
                f,
                "a {kind} `{name}` (line {line}) has no readable signature. \
                 Not comparing it: an empty signature reads as unchanged, which \
                 is the one conclusion a two-tree comparison must never reach by \
                 accident."
            ),
            Self::ParseErrors { count, first_line } => {
                write!(f, "{count} parser error node(s)")?;
                if let Some(l) = first_line {
                    write!(f, ", first at line {l}")?;
                }
                write!(
                    f,
                    ". Not reasoning about a file that does not parse; a gate \
                     here must report that it could not tell."
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// A parsed file, with everything the gates need already extracted.
///
/// Cloning is cheap: the heavy data is behind an `Arc`. Gates take this by
/// reference and are pure over it, which is what makes a gate testable
/// without a parser in the loop.
#[derive(Debug, Clone)]
pub struct ParsedFile {
    source: Arc<str>,
    items: Arc<Vec<PublicItem>>,
    unsafe_sites: Arc<Vec<UnsafeSite>>,
    suppressions: Arc<Vec<Suppression>>,
    tests: Arc<Vec<TestFn>>,
}

impl ParsedFile {
    /// The source, for a gate that needs to look at something not extracted.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Public items with their signatures, in source order.
    pub fn items(&self) -> &[PublicItem] {
        &self.items
    }

    /// Every `unsafe` site: blocks, declarations, impls, extern blocks.
    pub fn unsafe_sites(&self) -> &[UnsafeSite] {
        &self.unsafe_sites
    }

    /// Every diagnostic suppression, with the attribute it came from.
    pub fn suppressions(&self) -> &[Suppression] {
        &self.suppressions
    }

    /// Every test function, by identity.
    ///
    /// Identity rather than a count. `tests_not_deleted` compares the *set* of
    /// names across two trees, which catches a delete-and-replace pair that a
    /// count delta cannot see: remove one test and add another and the count
    /// is unchanged, while a test a human was relying on is gone.
    pub fn tests(&self) -> &[TestFn] {
        &self.tests
    }
}

/// A test function, identified by name and by where it lives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TestFn {
    /// Module path, so two same-named tests in different modules are distinct.
    pub path: String,
    /// The function name.
    pub name: String,
    /// 1-based line.
    pub line: u32,
    /// `#[ignore]`, so the test still exists but does not run.
    pub ignored: bool,
    /// `#[should_panic]`, which changes what passing means.
    pub should_panic: bool,
}

impl TestFn {
    /// A stable identity for comparing two trees.
    pub fn id(&self) -> String {
        if self.path.is_empty() {
            self.name.clone()
        } else {
            format!("{}::{}", self.path, self.name)
        }
    }
}

/// A public item's identity and signature.
///
/// The signature is the *normalised source text* of the declaration, not a
/// re-printed form. Normalising is what makes the comparison meaningful: a
/// reformatted function must compare equal to itself, and a renamed parameter
/// must not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicItem {
    /// A dotted path: `module::Type::method`, or a path-qualified trait.
    pub path: String,
    /// The declaration text, whitespace-normalised.
    pub signature: String,
    /// What kind of item it is, for the finding message.
    pub kind: ItemKind,
    /// 1-based line the declaration starts on.
    pub line: u32,
}

/// What kind of declaration an item is, so a finding can say what changed
/// rather than showing two signatures and asking the reader to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    /// A free function or an inherent method.
    Function,
    /// A struct declaration.
    Struct,
    /// An enum declaration.
    Enum,
    /// A trait declaration.
    Trait,
    /// An `impl Trait for Type`.
    TraitImpl,
    /// An `impl Type` with no trait.
    InherentImpl,
    /// A `type` alias.
    TypeAlias,
    /// A `const`.
    Constant,
    /// A `static`.
    Static,
    /// A `mod` declaration.
    Module,
    /// A `union` declaration.
    Union,
    /// An exported `macro_rules!`.
    Macro,
}

impl ItemKind {
    /// The word used in a finding message.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::TraitImpl => "trait impl",
            Self::InherentImpl => "inherent impl",
            Self::TypeAlias => "type alias",
            Self::Constant => "const",
            Self::Static => "static",
            Self::Module => "module",
            Self::Union => "union",
            Self::Macro => "macro",
        }
    }
}

/// A site where `unsafe` appears, classified because the three have different
/// consequences.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnsafeSite {
    /// Dotted path to the enclosing item.
    pub path: String,
    /// What kind of unsafe this is.
    pub kind: UnsafeKind,
    /// 1-based line.
    pub line: u32,
}

/// What kind of `unsafe` this is. The three have different consequences, and
/// reporting them as one number would hide which one grew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnsafeKind {
    /// An `unsafe` block inside a function.
    Block,
    /// An `unsafe fn`.
    Function,
    /// An `unsafe impl`.
    Impl,
    /// An `extern "C"` block or a foreign item.
    ExternBlock,
    /// An `unsafe trait`.
    Trait,
}

impl UnsafeKind {
    /// The word used in a finding message.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Function => "function",
            Self::Impl => "impl",
            Self::ExternBlock => "extern block",
            Self::Trait => "trait",
        }
    }
}

/// One diagnostic suppression: the lint suppressed, and where.
///
/// Suppression *count* is a weak signal; suppression *breadth* is the one that
/// matters. `#[allow(clippy::too_many_arguments)]` is narrow;
/// `#[allow(clippy::all)]` on a module is a project turning off its own
/// warnings, and `slop-gate` exists partly because that is a thing people do.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Suppression {
    /// Dotted path to the item the attribute is on.
    pub path: String,
    /// `allow` or `expect`.
    pub attribute: String,
    /// The lints named, or a whole-tool wildcard like `clippy::all`.
    pub lints: Vec<String>,
    /// Whether the attribute is on a module rather than an item, which is
    /// where a broad suppression does the most damage.
    pub module_wide: bool,
    /// 1-based line.
    pub line: u32,
}

impl Suppression {
    /// Whether this suppression covers every lint of a tool, e.g.
    /// `#[allow(clippy::all)]`. Distinguished from a specific list because
    /// broadening *to* a wildcard is the meaningful event.
    pub fn is_wildcard(&self) -> bool {
        self.lints
            .iter()
            .any(|l| l.ends_with("::all") || l == "all" || l == "warnings")
    }

    /// The width of what is suppressed, 0 = one lint, 1 = several,
    /// 2 = a whole tool. Used to order findings and to make a broadened
    /// wildcard a louder event than a broadened specific lint.
    pub fn breadth(&self) -> u8 {
        if self.is_wildcard() {
            2
        } else if self.lints.len() > 1 {
            1
        } else {
            0
        }
    }
}

/// Parse a Rust source file.
///
/// # Errors
///
/// Returns [`ParseError::ParseErrors`] if the source contains error or
/// missing nodes. It never returns a partially-trusted tree: a caller either
/// gets a sound parse or an error it must report as `Untrustworthy`.
pub fn parse(source: &str) -> Result<ParsedFile, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust_orchard::LANGUAGE.into())
        .map_err(|e| ParseError::GrammarUnavailable(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ParseError::GrammarUnavailable("parser returned no tree".into()))?;

    let root = tree.root_node();
    if root.has_error() {
        let mut count = 0usize;
        let mut first_line = None;
        let mut cursor = root.walk();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                count += 1;
                let line = node.start_position().row + 1;
                first_line = first_line.map_or(Some(line), |f: usize| Some(f.min(line)));
            }
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        return Err(ParseError::ParseErrors { count, first_line });
    }

    let mut collector = Collector::default();
    collector.walk(root, source, &mut Vec::new());
    // The invariant, enforced where the value is made rather than checked
    // where it is used. `ParsedFile` therefore cannot contain a `PublicItem`
    // with an empty signature, so a gate cannot report "no change" by
    // accident -- and no later code needs to remember to check.
    if let Some(bad) = collector
        .items
        .iter()
        .find(|i| i.signature.trim().is_empty())
    {
        return Err(ParseError::DegenerateSignature {
            kind: bad.kind.as_str(),
            name: bad.path.clone(),
            line: bad.line,
        });
    }
    // Belt and braces. The check above makes this unreachable, which is the
    // point: a `debug_assert` documents the invariant and costs nothing in
    // release, and if it ever fires the real check above has been removed.
    debug_assert!(
        collector
            .items
            .iter()
            .all(|i| !i.signature.trim().is_empty()),
        "a public item with an empty signature escaped the check above"
    );
    Ok(ParsedFile {
        source: Arc::from(source),
        items: Arc::new(collector.items),
        unsafe_sites: Arc::new(collector.unsafe_sites),
        suppressions: Arc::new(collector.suppressions),
        tests: Arc::new(collector.tests),
    })
}

/// A content-addressed parse cache, shared across the run.
///
/// One `Mutex<HashMap>` rather than an LRU: a run parses each file at most
/// twice (base and head), so the working set is already bounded by the size of
/// the change and an eviction policy would be machinery for nothing.
#[derive(Debug, Default)]
pub struct ParseCache {
    inner: Mutex<HashMap<ContentHash, CachedParse>>,
}

/// blake3 of the file's bytes. Keyed on content, never on path or mtime.
pub type ContentHash = [u8; 32];

/// A parse result, or the refusal to produce one, shared by every caller that
/// asks about the same bytes.
type CachedParse = Arc<Result<ParsedFile, ParseError>>;

impl ParseCache {
    /// A new, empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse, reusing a previous result for identical content.
    pub fn parse(&self, source: &str) -> CachedParse {
        let key = *blake3::hash(source.as_bytes()).as_bytes();
        let mut guard = self.inner.lock().expect("parse cache mutex poisoned");
        if let Some(hit) = guard.get(&key) {
            return Arc::clone(hit);
        }
        let parsed = Arc::new(parse(source));
        guard.insert(key, Arc::clone(&parsed));
        parsed
    }

    /// How many distinct sources have been parsed.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("parse cache mutex poisoned").len()
    }

    /// Whether nothing has been parsed yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---- collection ------------------------------------------------------------

#[derive(Debug, Default)]
struct Collector {
    items: Vec<PublicItem>,
    unsafe_sites: Vec<UnsafeSite>,
    suppressions: Vec<Suppression>,
    tests: Vec<TestFn>,
}

impl Collector {
    /// Walk the tree, tracking the module path so items are named by where
    /// they live rather than by their own text.
    fn walk(&mut self, node: tree_sitter::Node<'_>, source: &str, path: &mut Vec<String>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();
            let mut descend = true;

            // A declaration's attributes are its direct children, and the
            // declaration is one we will not descend into. So harvest them
            // here, where we know both the attributes and what they are
            // attached to — which is what tells us a module-wide suppression
            // from an item-wide one.
            if is_declaration(kind) {
                self.harvest_attributes(child, source, path, kind);
            }

            match kind {
                "function_item" | "function_signature_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        let sig = signature_of(child, source);
                        let vis = is_public(child, source);
                        if vis {
                            self.push_item(path, &name, sig, ItemKind::Function, child);
                        }
                        if let Some(test) = as_test(child, source, path, &name) {
                            self.tests.push(test);
                        }
                        if has_modifier(child, "unsafe") {
                            self.unsafe_sites.push(UnsafeSite {
                                path: join(path, &name),
                                kind: UnsafeKind::Function,
                                line: child.start_position().row as u32 + 1,
                            });
                        }
                        // The body is not part of the signature, but it can
                        // contain `unsafe` blocks, which are a separate gate.
                        if let Some(body) = child.child_by_field_name("body") {
                            let mut inner_path = path.to_vec();
                            inner_path.push(name.clone());
                            self.walk(body, source, &mut inner_path);
                        }
                        descend = false;
                    }
                }
                "struct_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Struct, child);
                        }
                        descend = false;
                    }
                }
                "enum_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Enum, child);
                        }
                        descend = false;
                    }
                }
                "union_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Union, child);
                        }
                        descend = false;
                    }
                }
                "trait_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Trait, child);
                        }
                        if has_modifier(child, "unsafe") {
                            self.unsafe_sites.push(UnsafeSite {
                                path: join(path, &name),
                                kind: UnsafeKind::Trait,
                                line: child.start_position().row as u32 + 1,
                            });
                        }
                        descend = false;
                    }
                }
                "type_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::TypeAlias, child);
                        }
                        descend = false;
                    }
                }
                "const_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Constant, child);
                        }
                        descend = false;
                    }
                }
                "static_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Static, child);
                        }
                        descend = false;
                    }
                }
                "mod_item" => {
                    if let Some(name) = ident_of(child, source, "name") {
                        if is_public(child, source) {
                            let sig = node_text(child, source);
                            self.push_item(path, &name, sig, ItemKind::Module, child);
                        }
                        // Only descend into inline module bodies; a `mod foo;`
                        // has no body here and its contents are another file.
                        let has_body = child
                            .child_by_field_name("body")
                            .is_some_and(|b| b.kind() == "declaration_list");
                        if has_body {
                            path.push(name);
                            self.walk(
                                child.child_by_field_name("body").expect("checked"),
                                source,
                                path,
                            );
                            path.pop();
                        }
                        descend = false;
                    }
                }
                "impl_item" => {
                    if let Some(trait_path) = child.child_by_field_name("trait") {
                        let type_path = child
                            .child_by_field_name("type")
                            .map(|t| node_text(t, source))
                            .unwrap_or_default();
                        let name = format!("{type_path} as {trait_path}");
                        let sig = node_text(child, source);
                        self.push_item(path, &name, sig, ItemKind::TraitImpl, child);
                    }
                    if has_modifier(child, "unsafe") {
                        self.unsafe_sites.push(UnsafeSite {
                            path: join(path, "<unsafe impl>"),
                            kind: UnsafeKind::Impl,
                            line: child.start_position().row as u32 + 1,
                        });
                    }
                    // The body holds methods, which are the interesting part.
                }
                "foreign_mod_item" | "extern_crate_declaration" => {
                    self.unsafe_sites.push(UnsafeSite {
                        path: join(path, "<extern>"),
                        kind: UnsafeKind::ExternBlock,
                        line: child.start_position().row as u32 + 1,
                    });
                    // Keep descending: the block's contents are items too.
                }
                "unsafe_block" => {
                    self.unsafe_sites.push(UnsafeSite {
                        path: join(path, "<block>"),
                        kind: UnsafeKind::Block,
                        line: child.start_position().row as u32 + 1,
                    });
                }
                "attributes" => {
                    // `attributes` is a wrapper; its children are the real
                    // `attribute_item` nodes.
                    let mut inner = child.walk();
                    for attr in child.children(&mut inner) {
                        if attr.kind() == "attribute_item" {
                            if let Some(s) = suppression_from(attr, source, path) {
                                self.suppressions.push(s);
                            }
                        }
                    }
                    descend = false;
                }
                _ => {}
            }

            if descend {
                self.walk(child, source, path);
            }
        }
    }

    /// Collect the suppressions on a declaration, recording whether the
    /// attribute covers a module.
    ///
    /// `#[allow(clippy::all)]` on a function silences one function. The same
    /// attribute on a `mod` silences everything beneath it, which is a
    /// different event with a very different cost, and the two are told apart
    /// by inspecting the declaration rather than by the attribute text.
    fn harvest_attributes(
        &mut self,
        node: tree_sitter::Node<'_>,
        source: &str,
        path: &[String],
        kind: &str,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() != "attributes" {
                continue;
            }
            let mut inner = child.walk();
            for attr in child.children(&mut inner) {
                if attr.kind() != "attribute_item" {
                    continue;
                }
                if let Some(mut s) = suppression_from(attr, source, path) {
                    s.module_wide = kind == "mod_item";
                    self.suppressions.push(s);
                }
            }
        }
    }

    fn push_item(
        &mut self,
        path: &[String],
        name: &str,
        signature: String,
        kind: ItemKind,
        node: tree_sitter::Node<'_>,
    ) {
        self.items.push(PublicItem {
            path: join(path, name),
            signature,
            kind,
            line: node.start_position().row as u32 + 1,
        });
    }
}

/// Every attribute on a declaration, parsed.
///
/// The attribute *path* is compared, not raw text, so `#[test]`,
/// `#[test = "x"]` and `#[tokio::test]` are all recognised and `#[testify]`
/// is not. Getting this wrong is not subtle: a naive equality against the text
/// after `#[` matches nothing at all, which makes a gate pass on every
/// repository while appearing calibrated.
fn attributes_of(node: tree_sitter::Node<'_>, source: &str) -> Vec<Attribute> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "attributes" {
            continue;
        }
        let mut inner = child.walk();
        for attr in child.children(&mut inner) {
            if attr.kind() == "attribute_item" {
                if let Some(a) = parse_attribute(attr, source) {
                    out.push(a);
                }
            }
        }
    }
    out
}

/// A test attribute, if the function carries one.
///
/// Recognised across the common frameworks rather than only bare `#[test]`,
/// because a gate that only knows `#[test]` reports "no tests removed" on a
/// repository that writes `#[tokio::test]` — the failure mode of a check that
/// has never met the codebase it runs on.
fn as_test(
    node: tree_sitter::Node<'_>,
    source: &str,
    path: &[String],
    name: &str,
) -> Option<TestFn> {
    let attrs = attributes_of(node, source);
    let has = |want: &str| attrs.iter().any(|a| a.last_segment() == want);
    if !(has("test") || has("test_case") || has("rstest") || has("bench")) {
        return None;
    }
    Some(TestFn {
        path: path.join("::"),
        name: name.to_string(),
        line: node.start_position().row as u32 + 1,
        ignored: has("ignore"),
        should_panic: has("should_panic"),
    })
}

/// The signature of a function: the declaration without its body, and without
/// its documentation.
///
/// A body change is not an API change, and including it would make every
/// implementation edit a finding. A *doc comment* change is not an API change
/// either, and the M5 corpus said so: on `pearls`, 7 of 8 "signature changed"
/// findings differed only in documentation prose, with a reader unable to see
/// which change was real. Doc comments are stripped for comparison, so a
/// change that lives only in a comment is not an API change.
///
/// The corpus check is the one to trust, because it cuts both ways: `0-4` to
/// `0-31` written in a doc comment *was* a real behavioural change that
/// happened to be recorded in a comment. A doc comment that contradicts the
/// code is a documentation bug, which is a different gate than this one and
/// belongs in `not_covered`.
/// The declaration without its body, with its documentation removed.
///
/// The body is cut by **byte range**, not by finding a `{` in the rendered
/// text. Finding a brace was wrong in two ways: a `where` clause or a const
/// generic argument can contain one, and an attribute's *value* can. Both made
/// the signature come out empty, which reads as "no change" and silently
/// disables the gate for that item. Found by
/// `a_real_signature_change_still_shows_through_doc_comments`.
fn signature_of(node: tree_sitter::Node<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |b| b.start_byte());
    let text = String::from_utf8_lossy(&bytes[start..end]);
    let stripped = strip_doc_comments(&text);
    if stripped.is_empty() && !text.trim().is_empty() {
        // A diagnostic aid that has already caught one bug and is cheap to
        // keep: a declaration whose signature came out empty is either a
        // parse oddity or a bug here, and an empty signature reads as "no
        // change", which silently disables the gate.
        eprintln!(
            "palisade-ast: signature_of produced nothing for {kind:?}: {text:?}",
            kind = node.kind()
        );
    }
    stripped
}

/// Remove `///`, `//!` and `/** */` comments from a declaration's text.
fn strip_doc_comments(text: &str) -> String {
    // Match the *longest* marker at each position. Taking the first match
    // among `///`, `//!`, `/**` and `/*` by position alone pairs `//` with the
    // `///` that starts it, then hunts for a block-comment close in what is
    // really a line comment -- which swallowed the rest of the declaration and
    // left an empty signature. An empty signature reads as "no change", so the
    // gate silently stopped working for every documented item.
    const MARKERS: [&str; 4] = ["///", "//!", "/**", "/*"];
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;

    while i < text.len() {
        let Some(marker_len) = MARKERS
            .iter()
            .find(|m| text[i..].starts_with(**m))
            .map(|m| m.len())
        else {
            let ch = text[i..].chars().next().expect("i is on a boundary");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        };
        let start = i + marker_len;
        if marker_len == 3 {
            // A line comment: ends at the newline.
            // `break` rather than assigning `i`: the loop is about to end and
            // the assignment would never be read.
            match text[start..].find('\n') {
                Some(nl) => i = start + nl,
                None => break,
            }
        } else {
            match text[start..].find("*/") {
                Some(close) => i = start + close + 2,
                None => break,
            }
        }
        // A stripped comment must not join the text on either side of it into
        // one token, or a `///` line above `pub fn` glues onto what follows.
        out.push(' ');
    }
    normalise(&out)
}
/// Whether a node kind is a declaration whose attributes we harvest.
fn is_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_signature_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "trait_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "mod_item"
            | "impl_item"
    )
}

fn join(path: &[String], name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", path.join("::"))
    }
}

/// A declaration's text, with its documentation removed.
///
/// Used for every kind, not just functions. A struct whose doc comment grew a
/// paragraph has not changed shape, and a report that says it has is a report
/// a reader stops believing.
fn node_text(node: tree_sitter::Node<'_>, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .map(strip_doc_comments)
        .unwrap_or_default()
}

/// Collapse whitespace so a reformatted declaration compares equal to itself.
///
/// This is the difference between "the signature changed" and "somebody ran
/// rustfmt". A gate that reports the second is a gate that gets switched off.
fn normalise(s: &str) -> String {
    // Two passes, because "is this comma trailing?" needs the next character
    // and a one-pass collapse does not have it.
    let collapsed = collapse_whitespace(s);
    strip_trailing_commas(&collapsed)
}

const NO_SPACE_BEFORE: &[char] = &[')', ']', '>', ';', ':'];
const NO_SPACE_AFTER: &[char] = &['(', '[', '<'];

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            last_space = true;
            continue;
        }
        // Drop the spaces that exist only because of where a line break fell.
        // Without this, `fn f(\n  a: i32,\n)` and `fn f(a: i32)` are different
        // signatures, and every rustfmt run is an API-change finding.
        if last_space
            && !out.is_empty()
            && !NO_SPACE_BEFORE.contains(&ch)
            && !out.ends_with(NO_SPACE_AFTER)
        {
            out.push(' ');
        }
        out.push(ch);
        last_space = false;
    }
    out
}

/// Remove commas that are immediately followed by a closing delimiter.
///
/// A trailing comma is a formatting choice, not part of the signature. It is
/// the other half of making `rustfmt` a non-event for this gate.
fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let next = chars[i + 1..]
                .iter()
                .copied()
                .find(|c| !c.is_whitespace() && *c != ',');
            if next.is_some_and(|c| matches!(c, ')' | ']' | '}')) {
                i += 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn ident_of(node: tree_sitter::Node<'_>, source: &str, field: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string())
}

/// Whether a declaration carries a modifier such as `unsafe`.
///
/// Two shapes, because the grammar has two: `unsafe fn f()` parses as a
/// direct `unsafe` token, while `pub unsafe fn f()` wraps it in a
/// `function_modifiers` node. Checking only the direct form silently misses
/// every `pub unsafe fn`, which is the most interesting case.
fn has_modifier(node: tree_sitter::Node<'_>, modifier: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| {
        if c.kind() == modifier {
            return true;
        }
        // A wrapper node such as `function_modifiers`.
        if c.kind().ends_with("_modifiers") {
            let mut inner = c.walk();
            return c.children(&mut inner).any(|g| g.kind() == modifier);
        }
        false
    })
}

/// Whether a declaration is visible outside its module.
fn is_public(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| {
        // A bare `pub` and nothing else. `pub(crate)`, `pub(super)` and
        // `pub(in path)` are not public API, and treating them as such would
        // make every internal-visibility item look like a public one.
        child.kind() == "visibility_modifier"
            && child
                .utf8_text(source.as_bytes())
                .is_ok_and(|t| t.trim() == "pub")
    })
}

/// A parsed attribute: its path and its arguments, with no judgement about
/// what the attribute *means*.
///
/// The judgement belongs to the caller, because "is this a diagnostic
/// suppression" and "is this a test" are different questions about the same
/// syntax, and a parser that decides both at once gets one of them wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// The attribute path as written, e.g. `allow` or `tokio::test`.
    pub path: String,
    /// Comma-separated arguments, quotes stripped.
    pub args: Vec<String>,
    /// 1-based line of the `#[`.
    pub line: u32,
}

impl Attribute {
    /// The last `::` segment, so `tokio::test` yields `test`.
    pub fn last_segment(&self) -> &str {
        self.path.rsplit("::").next().unwrap_or(&self.path)
    }
}

fn parse_attribute(node: tree_sitter::Node<'_>, source: &str) -> Option<Attribute> {
    let text = node_text(node, source);
    let inner = text.strip_prefix("#[")?.strip_suffix(']')?;
    let (path, args) = match inner.split_once(['(', '=']) {
        Some((a, rest)) => (a.trim(), rest.trim_end_matches(')')),
        None => (inner.trim(), ""),
    };
    Some(Attribute {
        path: path.to_string(),
        args: args
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_matches('"').to_string())
            .collect(),
        line: node.start_position().row as u32 + 1,
    })
}

/// The diagnostic attributes. `#[derive(..)]` and `#[test]` are not
/// suppressions, and treating them as such would make this gate fire on
/// every derive in the codebase.
fn is_diagnostic_attribute(path: &str) -> bool {
    matches!(path, "allow" | "expect" | "warn" | "deny" | "forbid")
}

fn suppression_from(
    node: tree_sitter::Node<'_>,
    source: &str,
    path: &[String],
) -> Option<Suppression> {
    let attr = parse_attribute(node, source)?;
    if !is_diagnostic_attribute(&attr.path) {
        return None;
    }
    Some(Suppression {
        path: path.join("::"),
        attribute: attr.path,
        lints: attr.args,
        // Set by the caller, which knows what the attribute is attached to.
        module_wide: false,
        line: attr.line,
    })
}
