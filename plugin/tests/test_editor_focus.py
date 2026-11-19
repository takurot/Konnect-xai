"""PCB Editor ownership for Konnect's modal settings and server actions."""

import types
import unittest
from unittest import mock

from test_pid_lifecycle import _load_plugin


class EditorFocusTests(unittest.TestCase):
    def test_settings_and_start_action_use_the_pcb_editor_as_parent(self):
        plugin = _load_plugin()
        manager = mock.Mock()
        manager.GetTitle.return_value = "demo — KiCad 10.0"
        manager.GetName.return_value = "frame"
        editor = mock.Mock()
        editor.GetTitle.return_value = "demo — PCB Editor"
        editor.GetName.return_value = "pcbnew"
        focus = object()
        plugin.wx = types.SimpleNamespace(
            GetTopLevelWindows=lambda: [manager, editor],
            Window=types.SimpleNamespace(FindFocus=lambda: focus),
            GetTopLevelParent=lambda _: editor,
            ID_YES=1,
            ID_NO=2,
        )
        plugin.KonnectSettingsDialog = mock.Mock()
        dialog = plugin.KonnectSettingsDialog.return_value
        dialog.ShowModal.return_value = plugin.wx.ID_YES
        plugin.start_server = mock.Mock()
        plugin.KonnectPlugin().Run()

        self.assertIs(plugin.KonnectSettingsDialog.call_args.kwargs["parent"], editor)
        plugin.start_server.assert_called_once_with(parent=dialog)
        manager.Raise.assert_not_called()

    def test_settings_stays_reachable_without_falling_back_to_the_manager(self):
        plugin = _load_plugin()
        manager = mock.Mock()
        manager.GetTitle.return_value = "demo — KiCad 10.0"
        manager.GetName.return_value = "frame"
        plugin.wx = types.SimpleNamespace(
            GetTopLevelWindows=lambda: [manager],
            Window=types.SimpleNamespace(FindFocus=lambda: None),
            ID_YES=1,
            ID_NO=2,
        )
        plugin.KonnectSettingsDialog = mock.Mock()
        plugin.KonnectSettingsDialog.return_value.ShowModal.return_value = None
        plugin.KonnectPlugin().Run()

        self.assertIsNone(plugin.KonnectSettingsDialog.call_args.kwargs["parent"])
        manager.Raise.assert_not_called()
