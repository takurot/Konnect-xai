"""Validate the release archive inventory and hash the exact bytes for publication."""

import hashlib
import json
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
        lines.append(f"{sha256(path)}  {name}\n")
    (directory / "SHA256SUMS").write_text("".join(lines), encoding="ascii")


def sha256(path: Path) -> str:
    with path.open("rb") as artifact:
        return hashlib.file_digest(artifact, "sha256").hexdigest()


def verify_draft(directory: Path, release: dict, tag: str) -> None:
    if release.get("isDraft") is not True or release.get("tagName") != tag:
        raise ValueError(f"expected a draft GitHub Release for {tag}")
    manifest = directory / "SHA256SUMS"
    expected = {}
    for line in manifest.read_text(encoding="ascii").splitlines():
        digest, name = line.split("  ", 1)
        expected[name] = f"sha256:{digest}"
    expected[manifest.name] = f"sha256:{sha256(manifest)}"

    assets = release["assets"]
    remote = {asset["name"]: asset for asset in assets}
    if len(remote) != len(assets):
        raise ValueError("duplicate GitHub Release asset names")
    if remote.keys() != expected.keys():
        missing = sorted(expected.keys() - remote.keys())
        unexpected = sorted(remote.keys() - expected.keys())
        raise ValueError(f"GitHub Release inventory mismatch: missing {missing}; unexpected {unexpected}")
    for name, digest in expected.items():
        asset = remote[name]
        if asset["state"] != "uploaded" or asset.get("digest") != digest:
            raise ValueError(f"GitHub Release asset upload state or digest mismatch: {name}")


if __name__ == "__main__":
    try:
        if len(sys.argv) == 4 and sys.argv[1] == "--verify-draft":
            verify_draft(Path(sys.argv[2]), json.load(sys.stdin), sys.argv[3])
        elif len(sys.argv) == 4:
            generate(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
        else:
            sys.exit(
                "usage: release-checksums.py DIRECTORY ARTIFACT_LABEL PCM_VERSION "
                "or release-checksums.py --verify-draft DIRECTORY TAG < release.json"
            )
    except (KeyError, OSError, ValueError) as error:
        sys.exit(str(error))
