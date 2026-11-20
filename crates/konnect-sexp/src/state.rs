//! Konnect's per-user state root. Project-owned files have their own paths.

use std::path::PathBuf;

pub struct StateDir {
    root: PathBuf,
    overridden: bool,
}

impl StateDir {
    pub fn resolve() -> Result<Self, &'static str> {
        if let Some(path) = std::env::var_os("KONNECT_STATE_DIR").filter(|value| !value.is_empty())
        {
            let root = PathBuf::from(path);
            if !root.is_absolute() {
                return Err("KONNECT_STATE_DIR must be an absolute path");
            }
            return Ok(Self {
                root,
                overridden: true,
            });
        }
        let root = dirs::data_local_dir()
            .filter(|path| path.is_absolute())
            .ok_or("no platform local-data directory is available; set KONNECT_STATE_DIR to an absolute path")?;
        Ok(Self {
            root: root.join("konnect"),
            overridden: false,
        })
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn is_overridden(&self) -> bool {
        self.overridden
    }

    /// Keep reading and updating legacy user data until it is moved explicitly.
    pub fn existing_or_default(
        &self,
        name: &str,
        legacy: impl FnOnce() -> Option<PathBuf>,
    ) -> PathBuf {
        let current = self.root.join(name);
        if !self.overridden
            && matches!(current.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            if let Some(previous) = legacy().filter(|path| path.is_absolute()) {
                if !matches!(previous.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                {
                    return previous;
                }
            }
        }
        current
    }
}

/// The pre-centralization state root used by user JSON preferences on Windows
/// and macOS, and by the catalog and templates on Windows.
pub fn legacy_roaming_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support");
    #[cfg(not(any(windows, target_os = "macos")))]
    return legacy_home_dir();
    #[cfg(any(windows, target_os = "macos"))]
    base.is_absolute().then(|| base.join("konnect"))
}

pub fn legacy_home_dir() -> Option<PathBuf> {
    let home = dirs::home_dir().filter(|path| path.is_absolute())?;
    Some(home.join(".konnect"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_ignores_old_data_and_default_checks_the_new_root_first() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        std::fs::write(&old, "old").unwrap();
        let current = root.path().join("config.json");
        let mut state = StateDir {
            root: root.path().to_owned(),
            overridden: true,
        };
        assert_eq!(
            state.existing_or_default("config.json", || panic!("override must isolate state")),
            current
        );
        state.overridden = false;
        assert_eq!(
            state.existing_or_default("config.json", || Some(old)),
            root.path().join("old")
        );
        std::fs::write(&current, "new").unwrap();
        assert_eq!(
            state.existing_or_default("config.json", || panic!("new state takes precedence")),
            current
        );
    }
}
