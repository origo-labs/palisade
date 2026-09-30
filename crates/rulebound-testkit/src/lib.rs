//! `rulebound-testkit` — fixture repositories, built deterministically.
//!
//! Dev-dependency only. Its whole job is to encode *how to make a dirty tree*
//! in exactly one place, because that is the knowledge the predecessor
//! programme got wrong and it is the thing the M0 regression test is
//! protecting.

use std::process::Command;

use camino::Utf8PathBuf;

/// A throwaway git repository with a deterministic identity and no surprises
/// from the developer's global config.
#[derive(Debug)]
pub struct FixtureRepo {
    /// The working directory of the fixture.
    pub path: Utf8PathBuf,
}

impl FixtureRepo {
    /// Create a repo under `root`, `git init`ed, with a fixed author and
    /// committer so a test never fails because `user.email` is unset on
    /// somebody's laptop.
    pub fn create_at(name: &str, root: &Utf8PathBuf) -> std::io::Result<Self> {
        let path = root.join(name);
        std::fs::create_dir_all(&path)?;
        let repo = Self { path };
        repo.git(&["init", "--initial-branch=main"])?;
        repo.git(&["config", "user.name", "rulebound test"])?;
        repo.git(&["config", "user.email", "test@rulebound.invalid"])?;
        repo.git(&["config", "commit.gpgsign", "false"])?;
        repo.git(&["config", "core.autocrlf", "false"])?;
        repo.git(&["config", "diff.noprefix", "false"])?;
        Ok(repo)
    }

    /// Run git in the fixture with a hermetic environment: no global or
    /// system config, fixed author and committer. A test must not depend on
    /// the developer's `~/.gitconfig`.
    ///
    /// # Errors
    ///
    /// Returns an error carrying stderr if git exits non-zero.
    pub fn git(&self, args: &[&str]) -> std::io::Result<String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "rulebound test")
            .env("GIT_AUTHOR_EMAIL", "test@rulebound.invalid")
            .env("GIT_COMMITTER_NAME", "rulebound test")
            .env("GIT_COMMITTER_EMAIL", "test@rulebound.invalid")
            .output()?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(std::io::Error::other(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )))
    }

    /// Write a file, creating parent directories. This is how a *dirty tree*
    /// is produced: write without staging.
    ///
    /// # Errors
    ///
    /// Propagates filesystem errors.
    pub fn write(&self, rel: &str, contents: &str) -> std::io::Result<()> {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(full, contents)
    }

    /// Delete a file from the worktree, leaving the deletion unstaged.
    ///
    /// # Errors
    ///
    /// Propagates filesystem errors.
    pub fn remove(&self, rel: &str) -> std::io::Result<()> {
        std::fs::remove_file(self.path.join(rel))
    }

    /// Stage everything and commit, returning the new SHA.
    ///
    /// The M0 regression test uses this to reproduce the one mistake that
    /// made a whole measurement programme score at chance: observing a state
    /// that has already been committed.
    ///
    /// # Errors
    ///
    /// Propagates git and filesystem errors.
    pub fn commit(&self, message: &str) -> std::io::Result<String> {
        self.git(&["add", "-A"])?;
        self.git(&["commit", "--no-verify", "-m", message])?;
        self.rev_parse("HEAD")
    }

    /// Resolve a rev to a full SHA.
    ///
    /// # Errors
    ///
    /// Propagates git errors.
    pub fn rev_parse(&self, rev: &str) -> std::io::Result<String> {
        Ok(self
            .git(&["rev-parse", "--verify", rev])?
            .trim()
            .to_string())
    }

    /// The baseline commit. Every two-tree gate compares against this.
    /// The current `HEAD`, as the baseline for a two-tree comparison.
    ///
    /// # Panics
    ///
    /// If the fixture has no `HEAD`, which means it was never committed.
    pub fn baseline(&self) -> String {
        self.rev_parse("HEAD").expect("fixture must have a HEAD")
    }

    /// Open the fixture through `rulebound-git`, the only crate that spawns
    /// git.
    ///
    /// # Panics
    ///
    /// If the path is not a repository, which would mean the fixture was
    /// built wrong.
    pub fn open(&self) -> rulebound_git::Repo {
        rulebound_git::Repo::open(&self.path).expect("fixture is a repository")
    }
}

/// A throwaway directory that cleans itself up. No `tempfile` dependency in
/// v1: the only requirement is a unique path and a `Drop`.
#[derive(Debug)]
pub struct TempDir {
    path: Utf8PathBuf,
}

impl TempDir {
    /// Create a uniquely named directory under the system temp dir.
    ///
    /// Uniqueness is process id, timestamp and a per-call counter, and
    /// `create_dir_all` is atomic, so a collision is an error rather than two
    /// tests sharing a directory.
    ///
    /// # Errors
    ///
    /// Propagates filesystem errors.
    pub fn new(label: &str) -> std::io::Result<Self> {
        // Process id plus a nanosecond timestamp plus a per-call counter kept
        // out of the name; `create_dir` is atomic, so a collision is an error
        // rather than a shared directory.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as u64 + d.as_secs());
        let base = std::env::temp_dir();
        let name = format!("rulebound-{label}-{}-{nanos}-{n}", std::process::id());
        let path = Utf8PathBuf::try_from(base.join(name))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// The directory path.
    pub fn path(&self) -> &Utf8PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
