//! Patch-chain resolution.
//!
//! A WoW installation is not one archive but a stack of them. The same path
//! commonly exists in several, and the client resolves it to whichever archive
//! sits highest in a fixed load order -- that is how patches replace base
//! content without rewriting multi-gigabyte files.

use std::path::{Path, PathBuf};

use crate::{Archive, Entry, Error};

/// A stack of archives searched from highest priority downwards.
pub struct Chain {
    /// Ordered lowest priority first, matching the client's load order.
    archives: Vec<Archive>,
}

impl Chain {
    pub fn new() -> Self {
        Self {
            archives: Vec::new(),
        }
    }

    /// Adds an archive at higher priority than everything already present.
    pub fn push(&mut self, archive: Archive) {
        self.archives.push(archive);
    }

    /// Opens the standard 3.3.5a archive set from a `Data` directory.
    ///
    /// Names are checked case-insensitively and missing members are skipped:
    /// installs vary in which optional patches are present, and Windows
    /// installs are inconsistent about capitalisation (`Patch-U.mpq` alongside
    /// `patch-3.MPQ`).
    pub fn open_wow_data(data_dir: impl AsRef<Path>, locale: &str) -> Result<Self, Error> {
        let data_dir = data_dir.as_ref();
        let mut order: Vec<PathBuf> = vec![
            data_dir.join("common.MPQ"),
            data_dir.join("common-2.MPQ"),
            data_dir.join("expansion.MPQ"),
            data_dir.join("lichking.MPQ"),
            data_dir.join("patch.MPQ"),
            data_dir.join("patch-2.MPQ"),
            data_dir.join("patch-3.MPQ"),
        ];

        // Locale archives outrank the base set, and lettered patches (the slot
        // private servers use for custom content) outrank the numbered ones.
        let loc = data_dir.join(locale);
        order.extend([
            loc.join(format!("locale-{locale}.MPQ")),
            loc.join(format!("expansion-locale-{locale}.MPQ")),
            loc.join(format!("lichking-locale-{locale}.MPQ")),
            loc.join(format!("base-{locale}.MPQ")),
            loc.join(format!("patch-{locale}.MPQ")),
            loc.join(format!("patch-{locale}-2.MPQ")),
            loc.join(format!("patch-{locale}-3.MPQ")),
        ]);
        for c in 'A'..='Z' {
            order.push(data_dir.join(format!("patch-{c}.MPQ")));
            order.push(loc.join(format!("patch-{locale}-{c}.MPQ")));
        }

        let mut chain = Self::new();
        for path in order {
            if let Some(actual) = resolve_case_insensitive(&path) {
                chain.push(Archive::open(actual)?);
            }
        }
        Ok(chain)
    }

    /// Index of the archive that wins for `name`, or `None` if the chain
    /// resolves it to nothing.
    ///
    /// The first archive with an opinion decides. That includes a delete
    /// marker: a patch that removes a file must mask the copy still sitting in
    /// the base archive below it, so the search stops rather than falling
    /// through.
    fn resolve(&self, name: &str) -> Option<usize> {
        self.archives
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, archive)| match archive.lookup(name) {
                crate::Lookup::Present(..) => Some(Some(i)),
                crate::Lookup::Deleted => Some(None),
                crate::Lookup::Absent => None,
            })
            .flatten()
    }

    fn owner(&mut self, name: &str) -> Option<&mut Archive> {
        let idx = self.resolve(name)?;
        self.archives.get_mut(idx)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }

    pub fn stat(&self, name: &str) -> Option<Entry> {
        self.archives[self.resolve(name)?].stat(name)
    }

    /// Reads a file, resolving it against the load order.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        self.owner(name)
            .ok_or_else(|| Error::NotFound(name.to_string()))?
            .read(name)
    }

    /// Reports which archive would win for `name`.
    pub fn source_of(&self, name: &str) -> Option<&Path> {
        Some(self.archives[self.resolve(name)?].path())
    }

    pub fn archives(&self) -> impl Iterator<Item = &Archive> {
        self.archives.iter()
    }

    /// Every archive's verdict on `name`, highest priority first, skipping
    /// archives with no entry. Diagnostic only.
    pub fn trace(&self, name: &str) -> Vec<(&Path, crate::State)> {
        self.archives
            .iter()
            .rev()
            .map(|a| (a.path(), a.state(name)))
            .filter(|(_, s)| *s != crate::State::Absent)
            .collect()
    }

    /// Union of every archive's `(listfile)`, deduplicated.
    ///
    /// These are names the archives *claim*, not names guaranteed to resolve.
    /// Stock installs ship listfile entries with no backing hash entry, and
    /// deleted files stay listed. Reconciling the two is `wow-cli verify`'s
    /// job, so this deliberately does not filter.
    pub fn list(&mut self) -> Result<Vec<String>, Error> {
        let mut all = Vec::new();
        for archive in &mut self.archives {
            all.extend(archive.list()?);
        }
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

/// Finds `path` ignoring case on its final component.
fn resolve_case_insensitive(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        return Some(path.to_path_buf());
    }
    let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
}
