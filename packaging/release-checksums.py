"""Validate the release archive inventory and hash the exact bytes for publication."""

import hashlib
from pathlib import Path
import re
import sys


SERVER_TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
)
PCM_PLATFORMS = ("linux", "macos", "windows")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?")


def generate(directory: Path, label: str, pcm_version: str) -> None:
    if not VERSION.fullmatch(pcm_version) or label not in (
        f"v{pcm_version}",
        f"v{pcm_version}-smoke",
    ):
        raise ValueError("release archive label and PCM version are inconsistent")
    expected = {
        f"konnect-{label}-{target}.{'zip' if target.endswith('msvc') else 'tar.gz'}"
        for target in SERVER_TARGETS
    } | {f"konnect-pcm-v{pcm_version}-{platform}.zip" for platform in PCM_PLATFORMS}
    entries = {entry.name: entry for entry in directory.iterdir()}
    if expected != entries.keys():
        missing = sorted(expected - entries.keys())
        unexpected = sorted(entries.keys() - expected)
        raise ValueError(f"release archive inventory mismatch: missing {missing}; unexpected {unexpected}")

    lines = []
    for name in sorted(expected):
        path = entries[name]
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"not a populated release archive: {path}")
        with path.open("rb") as archive:
            digest = hashlib.file_digest(archive, "sha256").hexdigest()
        lines.append(f"{digest}  {name}\n")
    (directory / "SHA256SUMS").write_text("".join(lines), encoding="ascii")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("usage: release-checksums.py DIRECTORY ARTIFACT_LABEL PCM_VERSION")
    try:
        generate(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except (OSError, ValueError) as error:
        sys.exit(str(error))
