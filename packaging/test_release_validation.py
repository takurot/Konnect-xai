"""Regression checks for the checksums and raw binary release gates."""

import hashlib
from pathlib import Path
from runpy import run_path
import sys
import tempfile
import unittest


PACKAGING = Path(__file__).resolve().parent
generate = run_path(PACKAGING / "release-checksums.py")["generate"]
smoke = run_path(PACKAGING / "smoke-release-binary.py")["smoke"]


class ReleaseArchiveTest(unittest.TestCase):
    def test_dispatch_inventories_and_hashes_exactly_the_archives(self):
        names = [
            "konnect-v0.0.0-smoke-aarch64-apple-darwin.tar.gz",
            "konnect-v0.0.0-smoke-aarch64-unknown-linux-gnu.tar.gz",
            "konnect-v0.0.0-smoke-x86_64-apple-darwin.tar.gz",
            "konnect-v0.0.0-smoke-x86_64-pc-windows-msvc.zip",
            "konnect-v0.0.0-smoke-x86_64-unknown-linux-gnu.tar.gz",
            "konnect-pcm-v0.0.0-linux.zip",
            "konnect-pcm-v0.0.0-macos.zip",
            "konnect-pcm-v0.0.0-windows.zip",
        ]
        with tempfile.TemporaryDirectory() as work:
            directory = Path(work)
            for name in names[:-1]:
                (directory / name).write_bytes(name.encode())
            manifest = directory / "SHA256SUMS"
            with self.assertRaisesRegex(ValueError, "missing.*windows.zip"):
                generate(directory, "v0.0.0-smoke", "0.0.0")
            self.assertFalse(manifest.exists())

            (directory / names[-1]).write_bytes(names[-1].encode())
            extra = directory / "extra.tar.gz"
            extra.write_bytes(b"must not be distributed")
            with self.assertRaisesRegex(ValueError, "unexpected.*extra.tar.gz"):
                generate(directory, "v0.0.0-smoke", "0.0.0")
            self.assertFalse(manifest.exists())
            extra.unlink()

            generate(directory, "v0.0.0-smoke", "0.0.0")
            expected = [
                f"{hashlib.sha256(name.encode()).hexdigest()}  {name}"
                for name in sorted(names)
            ]
            self.assertEqual(manifest.read_text().splitlines(), expected)


class ReleaseBinaryTest(unittest.TestCase):
    DRIVER = """
import json, sys
if '--version' in sys.argv:
    print('konnect ' + sys.argv[1])
elif sys.argv[2] != 'no-response':
    message = json.loads(sys.stdin.readline())
    assert message['method'] == 'initialize'
    print(json.dumps({
        'jsonrpc': '2.0',
        'id': message['id'],
        'result': {
            'protocolVersion': '2025-06-18',
            'serverInfo': {'version': sys.argv[2]},
        },
    }))
"""

    def test_smoke_checks_version_from_the_executable_and_the_protocol(self):
        smoke([sys.executable, "-c", self.DRIVER, "1.2.3", "1.2.3"], "1.2.3")
        with self.assertRaisesRegex(ValueError, "unexpected version"):
            smoke([sys.executable, "-c", self.DRIVER, "9.9.9", "1.2.3"], "1.2.3")
        with self.assertRaisesRegex(ValueError, "unexpected MCP initialize result"):
            smoke([sys.executable, "-c", self.DRIVER, "1.2.3", "9.9.9"], "1.2.3")
        with self.assertRaises(ValueError):
            smoke([sys.executable, "-c", self.DRIVER, "1.2.3", "no-response"], "1.2.3")
