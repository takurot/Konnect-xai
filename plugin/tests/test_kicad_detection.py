"""Tests for the settings dialog's platform-specific KiCad discovery."""

import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock


PLUGIN_DIR = Path(__file__).resolve().parents[1]


def load_settings_dialog():
    wx = types.ModuleType("wx")
    wx.Dialog = object
    sys.modules.setdefault("wx", wx)
    spec = importlib.util.spec_from_file_location(
        "settings_dialog_detection_under_test", PLUGIN_DIR / "settings_dialog.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeRegistryKey:
    def __init__(self, values=None, children=None):
        self.values = values or {}
        self.children = children or {}

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False


def fake_winreg(*installs):
    children = {
        f"KiCad {version}": FakeRegistryKey({
            "DisplayName": f"KiCad {version} (current user)",
            "DisplayVersion": version,
            "InstallLocation": install_dir,
        })
        for version, install_dir in installs
    }
    uninstall = FakeRegistryKey(children=children)
    module = types.ModuleType("winreg")
    module.HKEY_CURRENT_USER = "HKCU"
    module.HKEY_LOCAL_MACHINE = "HKLM"
    module.KEY_READ = 1
    module.KEY_WOW64_64KEY = 2
    module.KEY_WOW64_32KEY = 4

    def open_key(parent, name, *_args):
        if parent == "HKCU":
            return uninstall
        if parent == "HKLM":
            raise FileNotFoundError(name)
        return parent.children[name]

    module.OpenKey = open_key
    module.QueryInfoKey = lambda key: (len(key.children), 0, 0)
    module.EnumKey = lambda key, index: list(key.children)[index]
    module.QueryValueEx = lambda key, name: (key.values[name], 1)
    return module


class KiCadDetectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dialog = load_settings_dialog()

    def test_current_user_uninstall_record_is_discovered(self):
        install_dir = r"C:\Users\casey\AppData\Local\Programs\KiCad\10.0"
        registry = fake_winreg(("10.0.5", install_dir))
        expected = os.path.join(install_dir, "bin", "kicad-cli.exe")

        with mock.patch.object(self.dialog.sys, "platform", "win32"), \
                mock.patch.dict(sys.modules, {"winreg": registry}), \
                mock.patch.object(self.dialog.os.path, "isfile",
                                  side_effect=lambda path: path == expected):
            self.assertEqual(self.dialog.detect_kicad_cli(), expected)

    def test_registry_discovery_sorts_future_versions_numerically(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            old, future = home / "old", home / "future"
            for install in [old, future]:
                (install / "bin").mkdir(parents=True)
                (install / "bin" / "kicad-cli.exe").touch()
            registry = fake_winreg(("10.0.5", old), ("11.2.0", future))
            with mock.patch.object(self.dialog.sys, "platform", "win32"), \
                    mock.patch.dict(sys.modules, {"winreg": registry}):
                self.assertEqual(self.dialog.detect_kicad_cli(),
                                 str(future / "bin" / "kicad-cli.exe"))

    def test_root_fallback_sorts_version_folders_across_install_locations(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            local = home / "local"
            programs = home / "programs"
            nightly = programs / "KiCad" / "10.99" / "bin" / "kicad-cli.exe"
            old = local / "Programs" / "KiCad" / "10.0" / "bin" / "kicad-cli.exe"
            invalid = programs / "KiCad" / "99.0.backup" / "bin" / "kicad-cli.exe"
            for cli in [old, nightly, invalid]:
                cli.parent.mkdir(parents=True)
                cli.touch()
            with mock.patch.object(self.dialog.sys, "platform", "win32"), \
                    mock.patch.dict(self.dialog.os.environ,
                                    {"LOCALAPPDATA": str(local), "ProgramFiles": str(programs)}, clear=True), \
                    mock.patch.dict(sys.modules, {"winreg": None}):
                self.assertEqual(self.dialog.detect_kicad_cli(), str(nightly))


if __name__ == "__main__":
    unittest.main()
