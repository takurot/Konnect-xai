//! User-facing presentation of filesystem paths.

use std::borrow::Cow;
use std::path::Path;

/// Display ordinary Windows paths in the spelling users and KiCad use.
/// Retain extended-only names where removing the prefix would change their meaning.
pub fn display_path(path: &Path) -> Cow<'_, str> {
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        return strip_windows_verbatim(text);
    }
    path.to_string_lossy()
}

/// Convert ordinary verbatim drive/UNC spellings for portable library URIs.
pub fn strip_windows_verbatim(path: &str) -> Cow<'_, str> {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return Cow::Borrowed(path);
    };
    let ordinary = |components: &str| {
        components.split('\\').all(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(*prefix).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                });
            !reserved && !part.ends_with(['.', ' ']) && !part.contains(['/', ':'])
        })
    };
    if let Some(unc) = rest.strip_prefix("UNC\\") {
        if unc.split('\\').filter(|part| !part.is_empty()).count() >= 2 && ordinary(unc) {
            return Cow::Owned(format!(r"\\{unc}"));
        }
    } else if matches!(rest.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
        && ordinary(&rest[3..])
    {
        return Cow::Borrowed(rest);
    }
    Cow::Borrowed(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_drive_and_unc_paths_have_their_usual_spelling() {
        assert_eq!(
            strip_windows_verbatim(r"\\?\C:\pcb\main.kicad_pcb"),
            r"C:\pcb\main.kicad_pcb"
        );
        assert_eq!(
            strip_windows_verbatim(r"\\?\UNC\server\share\board"),
            r"\\server\share\board"
        );
        for special in [
            r"\\?\Volume{abc}\board",
            r"\\?\C:\pcb.\board",
            r"\\.\COM1",
            r"C:\normal\board",
        ] {
            assert_eq!(strip_windows_verbatim(special), special);
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_canonical_paths_display_without_the_verbatim_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        assert!(!display_path(&canonical).starts_with(r"\\?\"));
    }
}
