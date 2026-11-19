//! KiCad installation discovery.
//!
//! Keep executable and bundled-library discovery behind this module so every
//! caller sees the same installations in the same order.  Discovery is
//! intentionally read-only: Windows and KiCad already persist install state,
//! while an explicit Konnect configuration remains the durable override.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Version {
    major: u32,
    minor: u32,
}

impl Version {
    const DEFAULT: Self = Self {
        major: 10,
        minor: 0,
    };

    fn from_directory(name: &OsStr) -> Option<Self> {
        let (major, minor) = name.to_str()?.split_once('.')?;
        Some(Self {
            major: number(major)?,
            minor: number(minor)?,
        })
    }

    // kicad-cli reports a bare version such as 10.0.8 or 10.99.0-127-gabcd.
    fn from_report(report: &str) -> Option<Self> {
        let mut parts = report.trim().split('.');
        let version = Self {
            major: number(parts.next()?)?,
            minor: number(parts.next()?)?,
        };
        if let Some(patch) = parts.next() {
            number(patch.split(['-', '+']).next()?)?;
        }
        Some(version)
    }

    fn directory_name(self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

fn number(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn versioned_dirs(base: &Path) -> Vec<(Version, PathBuf)> {
    let mut directories: Vec<_> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = Version::from_directory(&entry.file_name())?;
            let path = entry.path();
            path.is_dir().then_some((version, path))
        })
        .collect();
    directories.sort_by(|(left_version, left_path), (right_version, right_path)| {
        right_version
            .cmp(left_version)
            .then_with(|| left_path.cmp(right_path))
    });
    directories
}

pub(crate) fn version_directories(base: &Path) -> impl Iterator<Item = PathBuf> {
    versioned_dirs(base).into_iter().map(|(_, path)| path)
}

pub(crate) fn versioned_environment(
    env: impl IntoIterator<Item = (OsString, OsString)>,
    suffix: &str,
) -> Vec<(u32, OsString)> {
    let mut matches: Vec<_> = env
        .into_iter()
        .filter_map(|(key, value)| {
            let (major, found_suffix) = key.to_str()?.strip_prefix("KICAD")?.split_once('_')?;
            if found_suffix != suffix {
                return None;
            }
            Some((number(major)?, value))
        })
        .collect();
    matches.sort_by_key(|(major, _)| std::cmp::Reverse(*major));
    matches
}

tokio::task_local! {
    // Library lookups can recurse into another table or follow a symbol source.
    // The selection must follow the future across threads and yields.
    pub(crate) static REQUEST_CONFIG_DIR: PathBuf;
}

pub(crate) fn config_dir() -> PathBuf {
    REQUEST_CONFIG_DIR
        .try_with(Clone::clone)
        .unwrap_or_else(|_| select_config_dir(&config_base(), None))
}

pub(crate) async fn resolve_config_dir(configured_cli: &str) -> PathBuf {
    let executable = crate::tools::cli::resolve_cli_executable(configured_cli);
    let version = if executable.as_os_str().is_empty() {
        None
    } else {
        crate::runtime_info::probe_kicad_version(&executable).await
    };
    select_config_dir(&config_base(), version.as_deref())
}

fn select_config_dir(base: &Path, version: Option<&str>) -> PathBuf {
    if let Some(version) = version.and_then(Version::from_report) {
        return base.join(version.directory_name());
    }
    version_directories(base)
        .next()
        .unwrap_or_else(|| base.join(Version::DEFAULT.directory_name()))
}

fn config_base() -> PathBuf {
    if let Some(base) = nonempty_env("KICAD_CONFIG_HOME") {
        return base;
    }
    #[cfg(target_os = "windows")]
    let base = nonempty_env("APPDATA").unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = nonempty_env("HOME")
        .unwrap_or_default()
        .join("Library/Preferences");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = nonempty_env("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| nonempty_env("HOME").unwrap_or_default().join(".config"));
    base.join("kicad")
}

fn nonempty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Find `kicad-cli`, honoring an explicit configured value first.
pub fn find_cli(configured: &str) -> Option<PathBuf> {
    resolve_configured(configured)
        .or_else(|| cli_candidates().into_iter().find(|path| path.is_file()))
}

/// Find the KiCad project-manager executable.
///
/// An explicit GUI path wins. If the configured CLI is an absolute path, its
/// sibling GUI is next so a selected KiCad version cannot be mixed with a
/// different installation's libraries or UI.
pub fn find_gui(configured_binary: &str, configured_cli: &str) -> Option<PathBuf> {
    resolve_explicit_path(configured_binary)
        .or_else(|| {
            resolve_configured(configured_cli)
                .and_then(|cli| installation_root_from_binary(&cli))
                .map(|root| root.join("bin").join(gui_filename()))
                .filter(|path| path.is_file())
        })
        .or_else(|| resolve_configured(configured_binary))
        .or_else(|| gui_candidates().into_iter().find(|path| path.is_file()))
}

fn cli_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        installation_roots()
            .into_iter()
            .map(|root| root.join("bin").join(cli_filename()))
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        let mut paths = vec![
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"),
            PathBuf::from("/usr/local/bin/kicad-cli"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(
                PathBuf::from(home).join("Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"),
            );
        }
        paths
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            PathBuf::from("/usr/bin/kicad-cli"),
            PathBuf::from("/usr/local/bin/kicad-cli"),
        ]
    }
}

fn gui_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        installation_roots()
            .into_iter()
            .map(|root| root.join("bin").join(gui_filename()))
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad",
        )]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        ["kicad"]
            .into_iter()
            .filter_map(|name| find_on_path(Path::new(name)))
            .collect()
    }
}

/// Existing roots that directly contain KiCad's `symbols`, `footprints`, and
/// `3dmodels` directories, newest supported KiCad first.
pub fn share_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    for install in installation_roots() {
        push_existing_unique(&mut roots, install.join("share").join("kicad"));
    }

    #[cfg(target_os = "macos")]
    {
        push_existing_unique(
            &mut roots,
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport"),
        );
        push_existing_unique(&mut roots, PathBuf::from("/usr/local/share/kicad"));
        push_existing_unique(&mut roots, PathBuf::from("/opt/homebrew/share/kicad"));
        if let Some(home) = std::env::var_os("HOME") {
            push_existing_unique(
                &mut roots,
                PathBuf::from(home).join("Applications/KiCad/KiCad.app/Contents/SharedSupport"),
            );
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        for root in [
            PathBuf::from("/usr/share/kicad"),
            PathBuf::from("/usr/local/share/kicad"),
            PathBuf::from("/opt/kicad/share/kicad"),
            PathBuf::from("/var/lib/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad"),
            PathBuf::from("/snap/kicad/current/usr/share/kicad"),
        ] {
            push_existing_unique(&mut roots, root);
        }
        if let Some(home) = std::env::var_os("HOME") {
            push_existing_unique(
                &mut roots,
                PathBuf::from(home).join(
                    ".local/share/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad",
                ),
            );
        }
    }

    if roots.is_empty() {
        static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::warn!(
                target: "konnect::kicad_install",
                "no KiCad bundled-library root was resolved; stock symbols, footprints, and 3D models may be unavailable; configure kicad_cli or the KICAD<major>_*_DIR variables"
            );
        });
    }

    roots
}

#[cfg(target_os = "windows")]
fn installation_roots() -> Vec<PathBuf> {
    static ROOTS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(discover_installation_roots).clone()
}

#[cfg(target_os = "windows")]
fn discover_installation_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in windows_registry_install_roots() {
        push_existing_unique(&mut roots, root);
    }
    for root in windows_well_known_install_roots(
        std::env::var_os("LOCALAPPDATA").as_deref(),
        std::env::var_os("ProgramFiles").as_deref(),
        std::env::var_os("ProgramFiles(x86)").as_deref(),
    ) {
        push_existing_unique(&mut roots, root);
    }
    roots
}

fn resolve_configured(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return None;
    }
    let path = PathBuf::from(configured);
    if path.is_file() {
        return Some(path);
    }
    if path.components().count() == 1 {
        return find_on_path(&path);
    }
    None
}

fn resolve_explicit_path(configured: &str) -> Option<PathBuf> {
    let path = PathBuf::from(configured.trim());
    (path.components().count() > 1 && path.is_file()).then_some(path)
}

fn find_on_path(filename: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(filename))
        .find(|path| path.is_file())
}

fn installation_root_from_binary(binary: &Path) -> Option<PathBuf> {
    let bin_dir = binary.parent()?;
    if !bin_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return None;
    }
    bin_dir.parent().map(Path::to_path_buf)
}

fn push_existing_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_dir() && !paths.contains(&path) {
        paths.push(path);
    }
}

#[cfg(any(target_os = "windows", test))]
fn cli_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "kicad-cli.exe"
    } else {
        "kicad-cli"
    }
}

fn gui_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "kicad.exe"
    } else {
        "kicad"
    }
}

#[cfg(target_os = "windows")]
fn windows_well_known_install_roots(
    local_app_data: Option<&std::ffi::OsStr>,
    program_files: Option<&std::ffi::OsStr>,
    program_files_x86: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(path) = local_app_data {
        bases.push(PathBuf::from(path).join("Programs").join("KiCad"));
    }
    if let Some(path) = program_files {
        bases.push(PathBuf::from(path).join("KiCad"));
    }
    if let Some(path) = program_files_x86 {
        bases.push(PathBuf::from(path).join("KiCad"));
    }
    for fallback in [
        r"C:\Program Files\KiCad",
        r"C:\KiCad",
        r"D:\KiCad",
        r"D:\Program Files\KiCad",
    ] {
        let fallback = PathBuf::from(fallback);
        if !bases.contains(&fallback) {
            bases.push(fallback);
        }
    }

    windows_install_candidates(&bases)
}

#[cfg(any(target_os = "windows", test))]
fn windows_install_candidates(bases: &[PathBuf]) -> Vec<PathBuf> {
    let mut versioned: Vec<_> = bases.iter().flat_map(|base| versioned_dirs(base)).collect();
    versioned.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    versioned
        .into_iter()
        .map(|(_, path)| path)
        .chain(bases.iter().cloned())
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_registry_install_roots() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let mut uninstall_roots = Vec::new();
    let mut legacy_roots = Vec::new();

    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let root = RegKey::predef(hive);
            if let Ok(uninstall) = root.open_subkey_with_flags(UNINSTALL, KEY_READ | view) {
                for name in uninstall.enum_keys().flatten() {
                    let Ok(key) = uninstall.open_subkey_with_flags(&name, KEY_READ | view) else {
                        continue;
                    };
                    let display_name: String = key.get_value("DisplayName").unwrap_or_default();
                    let lowercase = display_name.to_ascii_lowercase();
                    let Some(name_version) = lowercase.strip_prefix("kicad ") else {
                        continue;
                    };
                    let display_version: String =
                        key.get_value("DisplayVersion").unwrap_or_default();
                    let Some(version) = Version::from_report(&display_version)
                        .or_else(|| Version::from_report(name_version.split_whitespace().next()?))
                    else {
                        continue;
                    };
                    let location: String = key.get_value("InstallLocation").unwrap_or_default();
                    if !location.trim().is_empty() {
                        uninstall_roots.push((version, PathBuf::from(location.trim())));
                    }
                }
            }

            // Older KiCad installers wrote vendor keys instead of an uninstall entry.
            if let Ok(vendor) = root.open_subkey_with_flags(r"SOFTWARE\KiCad", KEY_READ | view) {
                for name in vendor.enum_keys().flatten() {
                    let version = if name.eq_ignore_ascii_case("KiCad") {
                        None
                    } else if let Some(version) = Version::from_directory(OsStr::new(&name)) {
                        Some(version)
                    } else {
                        continue;
                    };
                    let Ok(key) = vendor.open_subkey_with_flags(&name, KEY_READ | view) else {
                        continue;
                    };
                    let location: String = key
                        .get_value("InstallDir")
                        .or_else(|_| key.get_value(""))
                        .unwrap_or_default();
                    if !location.trim().is_empty() {
                        legacy_roots.push((version, PathBuf::from(location.trim())));
                    }
                }
            }
        }
    }

    uninstall_roots.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    legacy_roots.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
    uninstall_roots
        .into_iter()
        .map(|(_, path)| path)
        .chain(legacy_roots.into_iter().map(|(_, path)| path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn derives_install_root_from_cli() {
        let cli = Path::new(r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0\bin\kicad-cli.exe");
        assert_eq!(
            installation_root_from_binary(cli),
            Some(PathBuf::from(
                r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0"
            ))
        );
    }

    #[test]
    fn install_candidates_are_newest_first_across_windows_install_bases() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().join("per-user");
        let system = temp.path().join("system");
        for path in [
            user.join("10.99"),
            user.join("9.0"),
            system.join("11.0"),
            system.join("10.99"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        let roots = windows_install_candidates(&[user.clone(), system.clone()]);
        assert_eq!(
            roots,
            [
                system.join("11.0"),
                user.join("10.99"),
                system.join("10.99"),
                user.join("9.0"),
                user,
                system,
            ]
        );
    }

    #[test]
    fn config_selection_uses_report_then_newest_numeric_directory_then_default() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        assert_eq!(select_config_dir(base, None), base.join("10.0"));
        for name in [
            "9.0",
            "10.0",
            "10.99",
            "11.2",
            "11.12",
            "99.0.backup",
            "98.beta",
        ] {
            std::fs::create_dir(base.join(name)).unwrap();
        }
        std::fs::write(base.join("97.0"), "").unwrap();
        assert_eq!(select_config_dir(base, None), base.join("11.12"));
        assert_eq!(select_config_dir(base, Some("broken")), base.join("11.12"));
        assert_eq!(select_config_dir(base, Some("10.0.8")), base.join("10.0"));
        assert_eq!(
            select_config_dir(base, Some("10.99.0-127-gabcd")),
            base.join("10.99")
        );
        assert_eq!(select_config_dir(base, Some("11.13.0")), base.join("11.13"));
    }

    #[test]
    fn library_variables_recognize_future_majors_and_require_exact_names() {
        let vars = [
            ("KICAD9_FOOTPRINT_DIR", "nine"),
            ("KICAD11_FOOTPRINT_DIR", "eleven"),
            ("KICAD10_FOOTPRINT_DIR", "ten"),
            ("KICAD11_SYMBOL_DIR", "symbols"),
            ("KICAD11_FOOTPRINT_DIR_BACKUP", "backup"),
            ("KICAD11.1_FOOTPRINT_DIR", "malformed"),
        ];
        let found = versioned_environment(
            vars.map(|(key, value)| (key.into(), value.into())),
            "FOOTPRINT_DIR",
        );
        assert_eq!(
            found,
            [
                (11, "eleven".into()),
                (10, "ten".into()),
                (9, "nine".into())
            ]
        );
    }

    #[test]
    fn individual_discovery_sites_do_not_embed_the_old_version_list() {
        let obsolete = ["10.0", "9.0", "8.0"]
            .map(|version| format!("{version:?}"))
            .join(",");
        for source in [
            include_str!("tools/mod.rs"),
            include_str!("tools/integration.rs"),
        ] {
            let compact: String = source.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert!(!compact.contains(&obsolete));
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a complete KiCad installation"]
    fn live_install_exposes_all_bundled_library_kinds() {
        let cli = find_cli("").expect("KiCad installation was not discovered");
        let install = installation_root_from_binary(&cli).expect("CLI was not under <root>/bin");
        let share = install.join("share").join("kicad");

        for kind in ["symbols", "footprints", "3dmodels"] {
            assert!(share.join(kind).is_dir(), "missing KiCad {kind} directory");
        }
        assert!(share_roots().contains(&share));
    }

    #[test]
    fn explicit_cli_and_sibling_gui_win() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let cli = bin.join(cli_filename());
        let gui = bin.join(gui_filename());
        std::fs::write(&cli, b"").unwrap();
        std::fs::write(&gui, b"").unwrap();

        assert_eq!(find_cli(cli.to_str().unwrap()), Some(cli.clone()));
        assert_eq!(find_gui(gui_filename(), cli.to_str().unwrap()), Some(gui));
    }
}

#[cfg(test)]
mod discovery_contract_tests {
    use super::*;

    #[test]
    fn an_explicit_gui_path_wins_over_the_configured_cli_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("selected/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let cli = bin.join(cli_filename());
        let sibling = bin.join(gui_filename());
        let explicit = dir.path().join(gui_filename());
        for path in [&cli, &sibling, &explicit] {
            std::fs::write(path, b"fixture, never executed").unwrap();
        }
        assert_eq!(
            find_gui(explicit.to_str().unwrap(), cli.to_str().unwrap()),
            Some(explicit)
        );
    }

    #[test]
    fn explicit_paths_must_exist_and_installation_roots_must_have_a_bin_parent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing/cli");
        assert_eq!(resolve_configured(missing.to_str().unwrap()), None);
        assert_eq!(resolve_configured("  "), None);
        let existing = dir.path().join("tool");
        std::fs::write(&existing, b"not launched").unwrap();
        assert_eq!(
            resolve_explicit_path(existing.to_str().unwrap()),
            Some(existing.clone())
        );
        assert_eq!(installation_root_from_binary(&existing), None);
        assert_eq!(
            installation_root_from_binary(&dir.path().join("bin/tool")),
            Some(dir.path().to_path_buf())
        );
    }

    #[test]
    fn library_roots_include_existing_directories_once_and_preserve_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "not a library root").unwrap();
        let mut roots = Vec::new();
        for path in [
            first.clone(),
            file,
            first.clone(),
            second.clone(),
            dir.path().join("missing"),
        ] {
            push_existing_unique(&mut roots, path);
        }
        assert_eq!(roots, [first, second]);
    }
}
