//! The vault: one folder that is your whole presence on the neighborhood web.
//!
//! Everything you are and everything you share lives here. Hubs hold no copy.
//! Joining five hubs publishes this one folder five times over; it never
//! duplicates it, and leaving a hub takes nothing away from you.
//!
//! ```text
//! ~/.intraweb/
//!   identity.key   secret  your Ed25519 key -- back this up
//!   config.toml    private nickname and preferences
//!   intraweb.db    private known peers, hub history, mail
//!   site/          PUBLIC  your mini-site, served at /~nickname
//!   files/         PUBLIC  media and downloads peers may pull
//! ```

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Directory name used under the user's home when no explicit path is given.
const DEFAULT_DIR: &str = ".intraweb";
/// Environment variable that relocates the vault, for testing or USB keys.
pub const VAULT_ENV: &str = "INTRAWEB_VAULT";

#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the vault location: explicit path, then `$INTRAWEB_VAULT`, then
    /// `~/.intraweb`. The env var makes running two nodes on one machine easy,
    /// which is exactly what testing discovery requires.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        if let Some(path) = explicit {
            return Ok(Self::new(path));
        }
        if let Some(from_env) = std::env::var_os(VAULT_ENV) {
            return Ok(Self::new(PathBuf::from(from_env)));
        }
        let home = directories::UserDirs::new()
            .context("could not locate a home directory; pass --vault to choose one")?
            .home_dir()
            .to_path_buf();
        Ok(Self::new(home.join(DEFAULT_DIR)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn identity_path(&self) -> PathBuf {
        self.root.join("identity.key")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("intraweb.db")
    }

    /// Your mini-site. Public to every hub you join.
    pub fn site_dir(&self) -> PathBuf {
        self.root.join("site")
    }

    /// Files and media peers may browse and pull. Public to every hub you join.
    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    pub fn exists(&self) -> bool {
        self.identity_path().exists()
    }

    /// Create the folder layout if it is missing. Safe to call every start.
    pub fn ensure(&self) -> Result<()> {
        for dir in [self.root.clone(), self.site_dir(), self.files_dir()] {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        let landing = self.site_dir().join("index.html");
        if !landing.exists() {
            std::fs::write(&landing, STARTER_SITE)
                .with_context(|| format!("could not write {}", landing.display()))?;
        }
        Ok(())
    }
}

/// Placeholder so a freshly created vault serves something rather than a 404.
const STARTER_SITE: &str = r#"<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>A corner of the neighborhood web</title>
<style>
  body { font: 16px/1.6 system-ui, sans-serif; max-width: 34rem;
         margin: 12vh auto; padding: 0 1.5rem; color: #1c1b19; background: #faf8f5; }
  code { background: #ece8e1; padding: .15em .4em; border-radius: 4px; }
  @media (prefers-color-scheme: dark) {
    body { color: #e8e4dd; background: #17161a; }
    code { background: #2a282f; }
  }
</style>
<h1>Nobody has moved in yet.</h1>
<p>This page is the placeholder in your vault's <code>site/</code> folder.
   Replace it with anything &mdash; plain HTML, a photo album, a notice board.</p>
<p>Whatever you put here follows you to every hub you join. Drop files in
   <code>files/</code> to let neighbors browse and pull them.</p>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_the_public_folders() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().join("vault"));
        vault.ensure().unwrap();

        assert!(vault.site_dir().is_dir());
        assert!(vault.files_dir().is_dir());
        assert!(vault.site_dir().join("index.html").is_file());
    }

    #[test]
    fn ensure_does_not_clobber_an_existing_site() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path());
        vault.ensure().unwrap();
        std::fs::write(vault.site_dir().join("index.html"), "mine").unwrap();

        vault.ensure().unwrap();

        let kept = std::fs::read_to_string(vault.site_dir().join("index.html")).unwrap();
        assert_eq!(kept, "mine", "restart must never overwrite a user's site");
    }

    #[test]
    fn explicit_path_wins_over_environment() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("explicit");
        let vault = Vault::resolve(Some(chosen.clone())).unwrap();
        assert_eq!(vault.root(), chosen);
    }
}
