# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ffx.ui.session.py."""

import subprocess
import unittest
from unittest import mock

from honeydew.affordances.session import errors as session_errors
from honeydew.affordances.session import session_using_ffx
from honeydew.transports.ffx import errors as ffx_errors
from honeydew.transports.ffx import ffx as ffx_transport

_SESSION_STARTED = "Execution State:  Running"
_SESSION_STOPPED = ffx_errors.FfxCommandError(
    'No matching component instance found for query "core/session-manager/session:session"'
)
_TILE_URL = "fuchsia-pkg://fuchsia.com/foo#meta/bar.cm"


# pylint: disable=protected-access
class SessionFFXTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.ffx.ui.session.py."""

    def setUp(self) -> None:
        super().setUp()

        self.ffx_obj = mock.MagicMock(spec=ffx_transport.FFX)
        self.session_obj = session_using_ffx.SessionUsingFfx(
            device_name="fuchsia-emulator",
            ffx=self.ffx_obj,
            fuchsia_device_close=mock.MagicMock(),
        )

    def test_verify_supported(self) -> None:
        """Test if verify_supported works."""

    def test_start(self) -> None:
        """Test for Session.start() method."""
        self.ffx_obj.run.side_effect = ["", _SESSION_STARTED]

        self.session_obj.start()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "start"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1], mock.call(["session", "show"])
        )

    def test_start_double_start(self) -> None:
        """Test for Session.start() method."""
        self.ffx_obj.run.side_effect = [
            "",
            _SESSION_STARTED,
            "",
            _SESSION_STARTED,
        ]

        self.session_obj.start()
        self.session_obj.start()

        self.assertEqual(self.ffx_obj.run.call_count, 4)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "start"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "start"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[3], mock.call(["session", "show"])
        )

    def test_start_wait_for_started(self) -> None:
        """Test for Session.start() method. Verify it calls session show multiple time until session started"""
        self.ffx_obj.run.side_effect = ["", _SESSION_STOPPED, _SESSION_STARTED]

        self.session_obj.start()

        self.assertEqual(self.ffx_obj.run.call_count, 3)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "start"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )

    def test_start_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.start()."""
        self.ffx_obj.run.side_effect = ffx_errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.start()

        self.ffx_obj.run.assert_called_once_with(["session", "start"])

    def test_start_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.start()."""
        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.start()

        self.ffx_obj.run.assert_called_once_with(["session", "start"])

    def test_add_component(self) -> None:
        """Test for Session.add_component() method."""
        self.ffx_obj.run.side_effect = [_SESSION_STARTED, ""]

        self.session_obj.add_component(_TILE_URL)

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "add", _TILE_URL]),
        )

    def test_add_component_not_started_error(self) -> None:
        """Test for session not started in Session.add_component()."""
        self.ffx_obj.run.side_effect = [_SESSION_STOPPED]

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.add_component(_TILE_URL)

        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_add_component_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.add_component()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            ffx_errors.FfxCommandError("ffx error"),
        ]

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.add_component(_TILE_URL)

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "add", _TILE_URL]),
        )

    def test_add_component_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.add_component()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            subprocess.TimeoutExpired("ffx", 1),
        ]

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.add_component(_TILE_URL)

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "add", _TILE_URL]),
        )

    def test_stop(self) -> None:
        """Test for Session.stop() method."""
        self.ffx_obj.run.side_effect = ["", _SESSION_STOPPED]

        self.session_obj.stop()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "stop"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1], mock.call(["session", "show"])
        )

    def test_stop_wait_for_stopped(self) -> None:
        """Test for Session.stop() method."""
        self.ffx_obj.run.side_effect = ["", _SESSION_STARTED, _SESSION_STOPPED]

        self.session_obj.stop()

        self.assertEqual(self.ffx_obj.run.call_count, 3)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "stop"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )

    def test_stop_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.stop()."""
        self.ffx_obj.run.side_effect = ffx_errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.stop()

        self.ffx_obj.run.assert_called_once_with(["session", "stop"])

    def test_stop_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.stop()."""
        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.stop()

        self.ffx_obj.run.assert_called_once_with(["session", "stop"])

    def test_restart(self) -> None:
        """Test for Session.restart() method."""
        self.ffx_obj.run.side_effect = [_SESSION_STARTED, "", _SESSION_STARTED]

        self.session_obj.restart()
        self.assertEqual(self.ffx_obj.run.call_count, 3)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "restart"]),
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )

    def test_restart_wait_for_started(self) -> None:
        """Test for Session.restart() method."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            "",
            _SESSION_STOPPED,
            _SESSION_STARTED,
        ]

        self.session_obj.restart()
        self.assertEqual(self.ffx_obj.run.call_count, 4)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "restart"]),
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )

    def test_restart_not_started_error(self) -> None:
        """Test for session not started in Session.restart()."""
        self.ffx_obj.run.side_effect = [_SESSION_STOPPED]

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.restart()

        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_restart_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.restart()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            ffx_errors.FfxCommandError("ffx error"),
        ]

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.restart()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "restart"]),
        )

    def test_restart_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.restart()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            subprocess.TimeoutExpired("ffx", 1),
        ]

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.restart()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "restart"]),
        )

    def test_is_started_started(self) -> None:
        """Test for Session.is_started() method."""
        self.ffx_obj.run.side_effect = [_SESSION_STARTED]
        self.assertTrue(self.session_obj.is_started())
        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_is_started_stopped(self) -> None:
        """Test for Session.is_started() method."""
        self.ffx_obj.run.side_effect = [_SESSION_STOPPED]
        self.assertFalse(self.session_obj.is_started())
        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_is_started_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.is_started()."""
        self.ffx_obj.run.side_effect = ffx_errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.is_started()

        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_is_started_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.is_started()."""
        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.is_started()

        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_ensure_started_started(self) -> None:
        """Test for Session.ensure_started() method. Session already started."""
        self.ffx_obj.run.side_effect = [_SESSION_STARTED]
        self.session_obj.ensure_started()
        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_ensure_started_not_started(self) -> None:
        """Test for Session.ensure_started() method. Session not started."""
        self.ffx_obj.run.side_effect = [_SESSION_STOPPED, "", _SESSION_STARTED]
        self.session_obj.ensure_started()
        self.assertEqual(self.ffx_obj.run.call_count, 3)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["session", "start"]),
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2], mock.call(["session", "show"])
        )

    def test_cleanup_session_stopped(self) -> None:
        """Test for Session.cleanup() method. Session already stopped."""
        self.ffx_obj.run.side_effect = [_SESSION_STOPPED]
        self.session_obj._cleanup()
        self.ffx_obj.run.assert_called_once_with(["session", "show"])

    def test_cleanup(self) -> None:
        """Test for Session.cleanup() method."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            """component/unrelated
core/session-manager/session:session/elements:want-delete
core/session-manager/session:session/elements:main
core/session-manager/session:session/elements:main/unrelated
""",
            "",
        ]

        self.session_obj._cleanup()
        self.assertEqual(self.ffx_obj.run.call_count, 3)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["component", "list"]),
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[2],
            mock.call(["session", "remove", "want-delete"]),
        )

    def test_cleanup_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.cleanup()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            ffx_errors.FfxCommandError("ffx error"),
        ]
        with self.assertRaises(session_errors.SessionError):
            self.session_obj._cleanup()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["component", "list"]),
        )

    def test_cleanup_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.cleanup()."""
        self.ffx_obj.run.side_effect = [
            _SESSION_STARTED,
            subprocess.TimeoutExpired("ffx", 1),
        ]
        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj._cleanup()

        self.assertEqual(self.ffx_obj.run.call_count, 2)
        self.assertEqual(
            self.ffx_obj.run.call_args_list[0], mock.call(["session", "show"])
        )
        self.assertEqual(
            self.ffx_obj.run.call_args_list[1],
            mock.call(["component", "list"]),
        )


if __name__ == "__main__":
    unittest.main()
