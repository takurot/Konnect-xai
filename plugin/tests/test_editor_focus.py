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
        dialog.ShowModal.side_effect = lambda: plugin.KonnectSettingsDialog.call_args.kwargs[
            "server_action"](True, dialog)
        plugin.start_server = mock.Mock()
        plugin.KonnectPlugin().Run()

        self.assertIs(plugin.KonnectSettingsDialog.call_args.kwargs["parent"], editor)
        plugin.start_server.assert_called_once_with(parent=dialog)
        manager.Raise.assert_not_called()

    def test_unavailable_state_error_uses_the_settings_dialog_as_parent(self):
        plugin = _load_plugin()
        plugin._CACHE_DIR = None
        plugin.wx = types.SimpleNamespace(MessageBox=mock.Mock(), OK=1, ICON_ERROR=2)
        dialog = object()

        self.assertFalse(plugin.start_server(parent=dialog))

        plugin.wx.MessageBox.assert_called_once()
        self.assertIs(plugin.wx.MessageBox.call_args.kwargs.get("parent"), dialog)

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


class InPlaceToggleTests(unittest.TestCase):
    def test_start_keeps_modal_open_until_running_status_is_read(self):
        from test_kicad_detection import load_settings_dialog

        module = load_settings_dialog()
        dialog = module.KonnectSettingsDialog.__new__(module.KonnectSettingsDialog)
        dialog._collect_settings = mock.Mock(return_value={})
        dialog.settings_path = "fixture-settings.json"
        dialog._server_running = False
        dialog._server_action = mock.Mock(return_value=True)
        dialog._read_server_status = mock.Mock(side_effect=[False, True])
        dialog.start_stop_btn = mock.Mock()
        dialog._pending_toggle = None
        dialog.EndModal = mock.Mock()
        dialog._update_server_status = mock.Mock()

        with mock.patch.object(module, "save_settings"):
            dialog._on_start_stop(None)
        dialog._server_action.assert_called_once_with(True, dialog)
        dialog.EndModal.assert_not_called()
        dialog.start_stop_btn.Disable.assert_called_once()
        self.assertFalse(dialog._server_running)

        dialog._poll_server_status()
        self.assertTrue(dialog._server_running)
        dialog.start_stop_btn.Enable.assert_called_once()
        dialog._update_server_status.assert_called_once()
