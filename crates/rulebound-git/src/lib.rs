//! `rulebound-git` — the only crate permitted to spawn `git`.
//!
//! Everything above this takes data. That is what makes the gates unit
//! testable against fixtures with no repo and no subprocess (PLAN.md 2).
//!
//! Every operation here is read-only. Nothing in Rulebound ever writes to the
//! repository, and in particular nothing ever commits: the observation is
//! collected on a **dirty tree** because `git diff` reads unstaged changes
//! (EVIDENCE.md, apparatus bugs). See [`Repo::observation_inputs`].

use std::path::{Path, PathBuf};
use std::process::Command;

use camino::Utf8PathBuf;

/// Why a git invocation did not produce usable output.
///
/// The `Spawn`/`Failed` distinction is the same one the verdict algebra
/// makes between "ran and said no" and "could not run at all"
/// (`EVIDENCE.md` 5). A gate that cannot tell them apart reports a
/// repository problem as a rule violation, and every consumer of its output
/// inherits the confusion.
#[derive(Debug)]
pub enum GitError {
    /// The process could not be started at all: git is absent or not
    /// executable.
    Spawn {
        /// The program that could not be run.
        program: String,
        /// The underlying OS error.
        err: std::io::Error,
    },
    /// Non-zero exit. `stderr` is kept because for a gate tool the
    /// difference between "could not run" and "ran and said no" is the whole
    /// point (EVIDENCE.md 5).
    Failed {
        /// The argument list, space-joined, for the message.
        args: String,
        /// The exit code, or `None` if the process was killed by a signal.
        code: Option<i32>,
        /// Standard error, captured under `LC_ALL=C` so it stays matchable.
        stderr: String,
    },
    /// The path is not inside a git repository.
    NotARepository {
        /// The path that was tried.
        path: Utf8PathBuf,
    },
    /// The path is not valid UTF-8. Git speaks bytes; this crate speaks
    /// strings, and the conversion happens at the edge rather than silently
    /// in the middle.
    PathNotUtf8 {
        /// The offending path.
        path: PathBuf,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { program, err } => write!(f, "could not run `{program}`: {err}"),
            Self::Failed { args, code, stderr } => {
                let code = code.map_or_else(|| "signal".to_string(), |c| c.to_string());
                write!(f, "`git {args}` exited {code}: {stderr}")
            }
            Self::NotARepository { path } => write!(f, "{path} is not a git repository"),
            Self::PathNotUtf8 { path } => {
                write!(f, "path is not valid UTF-8: {}", path.display())
            }
        }
    }
}

impl std::error::Error for GitError {}

/// Result alias for the crate, so call sites read as one type family.
pub type Result<T> = std::result::Result<T, GitError>;

/// A repository handle. Cheap to clone; holds only a path.
#[derive(Debug, Clone)]
pub struct Repo {
    root: Utf8PathBuf,
}

impl Repo {
    /// Open an existing repository, discovering its root.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if `start` is not inside a repository, or if git
    /// is unavailable.
    pub fn open(start: &Utf8PathBuf) -> Result<Self> {
        let out = run(start, &["rev-parse", "--show-toplevel"])?;
        let root = Utf8PathBuf::from(out.trim());
        if !root.is_absolute() {
            // Older git can emit a relative toplevel when cwd is relative.
            return Err(GitError::NotARepository {
                path: start.clone(),
            });
        }
        Ok(Self { root })
    }

    /// The repository's top level, as an absolute path.
    pub fn root(&self) -> &Utf8PathBuf {
        &self.root
    }

    /// `git rev-parse --verify <rev>^{commit}` to a full 40-char SHA.
    ///
    /// This is what makes a baseline a content-addressed, immutable thing
    /// rather than a moving branch name (PLAN.md 1.2).
    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        let spec = format!("{rev}^{{commit}}");
        let out = run(&self.root, &["rev-parse", "--verify", "--quiet", &spec])?;
        Ok(out.trim().to_string())
    }

    /// Merge base of two revs, as a full SHA. The base of a two-tree
    /// comparison is not "the tip of main"; it is where this work diverged.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        let out = run(&self.root, &["merge-base", a, b])?;
        Ok(out.trim().to_string())
    }

    /// Porcelain v1 status: NUL-separated, so paths with spaces, quotes or
    /// newlines are unambiguous. `-z` is not a nicety here, it is the
    /// difference between a correct parse and a silently wrong one.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if git cannot be run, or emits a malformed entry.
    pub fn status_porcelain_z(&self) -> Result<Vec<StatusEntry>> {
        let out = run(
            &self.root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        parse_status_z(out.as_bytes())
    }

    /// Changes between the working tree and `base` — or between the working
    /// tree and the index, when `base` is `None`.
    ///
    /// Untracked files are **not** in `git diff` output however they are
    /// spelled: `-u`/`-U` is unified context width, and `diff` has no
    /// `--untracked-files`. An observation that omits them is an observation
    /// of nothing where the work usually is, and every gate that reads a new
    /// file's content would be silently blind. They are synthesised with
    /// `--no-index` and labelled, so their provenance stays visible in the
    /// diff itself.
    pub fn diff_unstaged(&self, base: Option<&str>) -> Result<String> {
        let mut args: Vec<&str> = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-U3",
            "--find-renames",
        ];
        if let Some(b) = base {
            args.push(b);
        }
        let mut out = run(&self.root, &args)?;
        out.push_str(&self.diff_untracked()?);
        Ok(out)
    }

    /// Untracked files, as a diff against `/dev/null`.
    ///
    /// `--no-index` exits 1 when the files differ, which here is the
    /// *expected* outcome and not a failure. Exit 2 is git's own "trouble"
    /// code, so the split is on git's documented contract rather than on
    /// string-matching stderr for a message that is translated by locale.
    fn diff_untracked(&self) -> Result<String> {
        let mut out = String::new();
        for entry in self.status_porcelain_z()? {
            if !entry.is_untracked() {
                continue;
            }
            // A directory has no content to diff. Skip it rather than failing
            // the entire observation over one.
            if self.root.join(&entry.path).is_dir() {
                continue;
            }
            let chunk = run_allowing(
                &self.root,
                &[
                    "diff",
                    "--no-index",
                    "--no-color",
                    "-U3",
                    "/dev/null",
                    &entry.path,
                ],
                1,
            )?;
            out.push_str(&chunk);
        }
        Ok(out)
    }

    /// Staged changes: index versus HEAD. Kept separate from
    /// [`Repo::diff_unstaged`] because `git status` can show a file as changed
    /// while a plain `git diff` says nothing, and the predecessor's benchmark
    /// went blind on exactly that (`EVIDENCE.md`, apparatus bugs).
    pub fn diff_staged(&self) -> Result<String> {
        run(
            &self.root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "-U3",
                "--find-renames",
                "--cached",
            ],
        )
    }

    /// Contents of `path` at `rev`. The base side of every two-tree gate.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for a path absent at `rev` — an intentionally
    /// deleted file is a normal result, not a broken repository. Any other
    /// git failure propagates.
    pub fn show(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let spec = format!("{rev}:{path}");
        match run(&self.root, &["show", &spec]) {
            Ok(text) => Ok(Some(text)),
            // `git show` exits non-zero both for "absent" and for real
            // failures. Only the latter may be surfaced, or an intentionally
            // deleted file would look like a broken repository. Distinguishing
            // them requires the error text, so it is checked rather than
            // assumed.
            Err(GitError::Failed { ref stderr, .. }) => {
                if stderr.contains("exists on disk, but not in")
                    || stderr.contains("does not exist in")
                    || stderr.contains("path '")
                    || stderr.contains("exists in the index")
                {
                    Ok(None)
                } else {
                    Err(GitError::Failed {
                        args: format!("show {spec}"),
                        code: None,
                        stderr: stderr.clone(),
                    })
                }
            }
            Err(e) => Err(e),
        }
    }

    /// A tracked file's worktree content, or `None` if absent.
    pub fn worktree_file(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// True when the worktree has no modifications of any kind.
    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.status_porcelain_z()?.is_empty())
    }

    /// Per-file content cap for the two-tree view. Separate from the
    /// observation's diff budget because they bound different things: the
    /// diff budget bounds *reviewable evidence*, and this bounds the working
    /// set a gate may load into memory. One giant generated file should not
    /// be able to exhaust the second while saying nothing about the first.
    pub const FILE_VIEW_CAP: usize = 256 * 1024;

    /// Everything a two-tree observation needs, fetched once.
    ///
    /// Note what is *absent*: no commit, no add, no write. The whole class of
    /// bug this milestone exists to prevent starts with "commit the state so
    /// the diff is convenient", after which the diff is empty and the tool
    /// reports success while having observed nothing.
    pub fn observation_inputs(&self, base_ref: Option<&str>) -> Result<ObservationInputs> {
        let base = match base_ref {
            Some(r) => Some(self.rev_parse(r)?),
            None => None,
        };
        let status = self.status_porcelain_z()?;
        let dirty = !status.is_empty();
        let unstaged = self.diff_unstaged(base.as_deref())?;
        let staged = self.diff_staged()?;
        let files = self.file_views(base.as_deref(), &status)?;
        Ok(ObservationInputs {
            base,
            status,
            dirty,
            unstaged,
            staged,
            files,
        })
    }

    /// Both sides of every file that differs between `base` and the worktree.
    ///
    /// The set comes from `git diff --name-status <base>`, **not** from
    /// `git status`. Status is worktree-versus-index, so it is blind to work
    /// that has already been committed — and committed work is precisely the
    /// case the two-tree model exists to cover (PLAN.md 1.2). An earlier
    /// version of this function built the list from status and the
    /// end-to-end tests caught it: after a commit, the view was empty and
    /// every gate saw nothing. Untracked files are unioned in from status,
    /// since `diff` does not report them either.
    ///
    /// One `git show` per file rather than a batch: the set is bounded by the
    /// size of the change under review, and a batch reader would mean parsing
    /// a length-prefixed protocol to save a handful of process spawns on a run
    /// whose budget is dominated by `cargo test` anyway. Revisit if M5's
    /// per-commit budget measurement says otherwise.
    fn file_views(&self, base: Option<&str>, status: &[StatusEntry]) -> Result<Vec<RawFile>> {
        let mut entries: Vec<(String, Option<String>)> = match base {
            Some(rev) => self
                .changed_paths(rev)?
                .into_iter()
                .map(|(path, orig_path, _kind)| (path, orig_path))
                .collect(),
            None => status
                .iter()
                .map(|e| (e.path.clone(), e.orig_path.clone()))
                .collect(),
        };
        // Untracked files are in status but never in `diff`.
        for e in status.iter().filter(|e| e.is_untracked()) {
            if !entries.iter().any(|(p, _)| *p == e.path) {
                entries.push((e.path.clone(), e.orig_path.clone()));
            }
        }

        let mut out = Vec::with_capacity(entries.len());
        for (path, orig_path) in entries {
            let entry_path = path.clone();
            let entry_orig = orig_path.clone();
            if self.root.join(&entry_path).is_dir() || is_build_output(&entry_path) {
                continue;
            }
            let mut base_content = match base {
                Some(rev) => self.show(rev, &entry_path)?,
                None => None,
            };
            // A rename's base side lives at the *original* path, so asking
            // for the destination finds nothing. A copy, by contrast, is
            // already present at the destination.
            if base_content.is_none() {
                if let (Some(rev), Some(orig)) = (base, &entry_orig) {
                    base_content = self.show(rev, orig)?;
                }
            }
            let head_content = self.worktree_file(&entry_path);
            let truncated = base_content
                .as_ref()
                .is_some_and(|c| c.len() > Self::FILE_VIEW_CAP)
                || head_content
                    .as_ref()
                    .is_some_and(|c| c.len() > Self::FILE_VIEW_CAP);
            out.push(RawFile {
                path: entry_path,
                orig_path: entry_orig,
                base: base_content.map(|c| Self::cap(&c)),
                head: head_content.map(|c| Self::cap(&c)),
                truncated,
            });
        }
        Ok(out)
    }

    /// Paths that differ between `rev` and the worktree, with the status
    /// letter that says how and, for a rename, the original path.
    ///
    /// `git diff --name-status -z` is NUL-separated, so a path containing a
    /// space, a quote or a newline survives without unescaping. A rename or
    /// copy emits a status, then the original path, then the destination.
    fn changed_paths(&self, rev: &str) -> Result<Vec<(String, Option<String>, char)>> {
        let out = run_bytes(
            &self.root,
            &["diff", "--name-status", "-z", "--find-renames", rev],
        )?;
        let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
        let mut result = Vec::new();
        while let Some(status) = fields.next() {
            if status.is_empty() {
                continue;
            }
            // The status is a letter, optionally followed by a similarity
            // score: `R100`.
            let kind = status[0] as char;
            let (orig, dest) = match kind {
                'R' | 'C' => {
                    let Some(orig) = fields.next() else { break };
                    let Some(dest) = fields.next() else { break };
                    (
                        String::from_utf8_lossy(orig).into_owned(),
                        String::from_utf8_lossy(dest).into_owned(),
                    )
                }
                _ => {
                    let Some(path) = fields.next() else { break };
                    (
                        String::from_utf8_lossy(path).into_owned(),
                        String::from_utf8_lossy(path).into_owned(),
                    )
                }
            };
            let orig_path = (orig != dest).then_some(orig);
            result.push((dest, orig_path, kind));
        }
        Ok(result)
    }

    /// Truncate on a char boundary, so a gate never receives a broken string.
    fn cap(s: &str) -> String {
        if s.len() <= Self::FILE_VIEW_CAP {
            return s.to_string();
        }
        let mut end = Self::FILE_VIEW_CAP;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }
}

/// What `rulebound-observe` consumes. The boundary between "ran git" and
/// "reasoned about the result", so that the budget and the clipping rules are
/// testable with no repository present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationInputs {
    /// Resolved base commit, if a base was requested. A full SHA, so the
    /// baseline is content-addressed rather than a moving branch name.
    pub base: Option<String>,
    /// Parsed `git status --porcelain -z`.
    pub status: Vec<StatusEntry>,
    /// Whether the worktree differs from the index in any way.
    pub dirty: bool,
    /// Base-vs-worktree diff, including synthesised untracked content.
    pub unstaged: String,
    /// Index-vs-HEAD diff. Kept separate because a bare `git diff` is silent
    /// about staged work.
    pub staged: String,
    /// Both sides of every changed file, for the two-tree gates.
    pub files: Vec<RawFile>,
}

/// One file's content at the base and in the worktree, as fetched.
///
/// Named `RawFile` because `rulebound-observe` owns the canonical
/// `FileView` that gates actually consume, and this crate sits below it and
/// cannot name that type. The conversion is field-for-field, and the
/// distinction is deliberate: this is "what git gave us", that is "what a gate
/// is allowed to reason about".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    /// Repository-relative path; for a rename, the destination.
    pub path: String,
    /// Original path of a rename.
    pub orig_path: Option<String>,
    /// Content at the base commit, or `None` if absent there.
    pub base: Option<String>,
    /// Content in the worktree, or `None` if absent.
    pub head: Option<String>,
    /// Whether either side hit the per-file cap, or was not valid UTF-8.
    pub truncated: bool,
}

/// One `git status --porcelain -z` entry, path already unquoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The staged (index vs HEAD) status character, or `?` for untracked.
    pub x: char,
    /// The worktree status character, or ` ` when not applicable.
    pub y: char,
    /// The repository-relative path, literal bytes decoded lossily.
    pub path: String,
    /// Second path of a rename/copy in `porcelain -z`; `path` holds the
    /// destination.
    pub orig_path: Option<String>,
}

impl StatusEntry {
    /// A path present in the worktree that is not in the index.
    pub const fn is_untracked(&self) -> bool {
        self.x == '?'
    }
}

/// Build output, which is never source and never a reviewable change.
///
/// `cargo` writes here every time it runs, and a `Delegated` gate runs cargo.
/// Without this, the first `rulebound check` leaves a `target/` directory
/// behind and every subsequent run observes sixty-odd files that no reviewer
/// wants and no gate should read.
///
/// This is not a workaround for the gate writing to the tree — every Rust
/// project expects that, and pointing `CARGO_TARGET_DIR` elsewhere would
/// throw away the incremental cache that PRD 9's criterion 2 is measured
/// against. It is a statement that a build artefact is not a change to the
/// codebase.
fn is_build_output(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p == "target" || p.starts_with("target/") || p.starts_with(".cargo-target/")
}

fn parse_status_z(bytes: &[u8]) -> Result<Vec<StatusEntry>> {
    // `porcelain -z` is NUL-separated with no quoting at all: every field is
    // literal bytes. That is why this parser can be a split rather than a
    // shellwords/unescape dance, and it is why a path containing a quote or a
    // newline cannot corrupt the parse.
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut out = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 3 {
            return Err(GitError::Failed {
                args: "status --porcelain -z".to_string(),
                code: None,
                stderr: format!("malformed status entry: {entry:?}"),
            });
        }
        let (x, y) = (entry[0] as char, entry[1] as char);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        // A rename or copy is `R`/`C` in x, and -z emits the original path as
        // the *next* field.
        let orig_path = if x == 'R' || x == 'C' {
            fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
        } else {
            None
        };
        out.push(StatusEntry {
            x,
            y,
            path,
            orig_path,
        });
    }
    Ok(out)
}

/// The single place in the workspace that spawns a process (PLAN.md 2,
/// boundary 1). `cwd` is a `&Utf8PathBuf` rather than a `&Path` so a
/// non-UTF-8 repository path is an error at the edge rather than a lossy
/// conversion into git's argv.
fn run_bytes(cwd: &Utf8PathBuf, args: &[&str]) -> Result<Vec<u8>> {
    let out = spawn(cwd, args)?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    Err(GitError::Failed {
        args: args.join(" "),
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The single `Command::new("git")` in the workspace.
fn spawn(cwd: &Utf8PathBuf, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Locale must not leak into parseable output. An error message
        // translated into the user's language is not matchable, and a
        // `git show` "does not exist" message we fail to recognise becomes a
        // spurious repository error.
        .env("LC_ALL", "C")
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|err| GitError::Spawn {
            program: "git".to_string(),
            err,
        })
}

/// Like [`run`], but treats exit code `expected` as success, returning stdout
/// either way.
///
/// Used only where a non-zero exit is the documented meaning of "yes", which
/// is `git diff --no-index` (1 = files differ). Exit 2 stays an error: that is
/// git's own "trouble" code, so a genuine failure can never be mistaken for a
/// diff that happened to be empty.
fn run_allowing(cwd: &Utf8PathBuf, args: &[&str], expected: i32) -> Result<String> {
    let out = spawn(cwd, args)?;
    if out.status.success() || out.status.code() == Some(expected) {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(GitError::Failed {
        args: args.join(" "),
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn run(cwd: &Utf8PathBuf, args: &[&str]) -> Result<String> {
    run_bytes(cwd, args).map(|b| String::from_utf8_lossy(&b).into_owned())
}

impl Repo {
    /// Open a path, or explain that it is not a repository. Used by the CLI
    /// so a user in the wrong directory gets a sentence, not an exit code.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::PathNotUtf8`] for a non-UTF-8 path, or whatever
    /// [`Repo::open`] returns.
    pub fn open_path(p: &Path) -> Result<Self> {
        let s = p.to_str().ok_or_else(|| GitError::PathNotUtf8 {
            path: p.to_path_buf(),
        })?;
        Self::open(&Utf8PathBuf::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_z_parses_spaces_and_quotes_literally() {
        // The whole reason for -z: a path with a space, a quote and a
        // newline must survive intact rather than needing unescaping.
        // Real `porcelain -z` layout is `XY<space>PATH\0`, with the original
        // path of a rename as the next NUL-separated field.
        let raw = b"?? src/has space/it's \"quoted\".rs\0R  src/new.rs\0src/old.rs\0";
        let entries = parse_status_z(raw).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "src/has space/it's \"quoted\".rs");
        assert!(entries[0].is_untracked());
        assert_eq!(entries[1].x, 'R');
        assert_eq!(entries[1].y, ' ');
        assert_eq!(entries[1].path, "src/new.rs");
        assert_eq!(entries[1].orig_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn status_z_handles_path_with_newline() {
        let raw = b"?? a\nb.rs\0";
        let entries = parse_status_z(raw).unwrap();
        assert_eq!(entries[0].path, "a\nb.rs");
    }

    #[test]
    fn status_z_rejects_a_truncated_entry() {
        assert!(parse_status_z(b"?\0").is_err());
    }

    #[test]
    fn status_z_ignores_trailing_empty_field() {
        assert_eq!(parse_status_z(b"?? a.rs\0\0").unwrap().len(), 1);
    }
}

#[cfg(test)]
mod build_output_tests {
    use super::is_build_output;

    #[test]
    fn build_output_is_not_a_reviewable_change() {
        // A `Delegated` gate runs cargo, which writes here. If these entered
        // the observation, the first `rulebound check` would leave the next one
        // looking at sixty build artefacts.
        for p in [
            "target",
            "target/debug/demo",
            "target/CACHEDIR.TAG",
            ".cargo-target/x",
        ] {
            assert!(is_build_output(p), "{p} should be excluded");
        }
    }

    #[test]
    fn source_that_merely_looks_like_build_output_is_kept() {
        // `targets/` is a real directory somebody might mean. So is a module
        // called `target.rs`.
        for p in [
            "src/target.rs",
            "src/target/mod.rs",
            "targets/a.rs",
            "src/targeting.rs",
            "Cargo.lock",
            "src/lib.rs",
        ] {
            assert!(!is_build_output(p), "{p} should be observed");
        }
    }
}
