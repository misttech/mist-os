# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.ffx.ui.session.py."""

import subprocess
import unittest
from unittest import mock

from honeydew import errors
from honeydew.affordances.session import errors as session_errors
from honeydew.affordances.session import session_using_ffx
from honeydew.transports import ffx as ffx_transport

TILE_URL = "fuchsia-pkg://fuchsia.com/foo#meta/bar.cm"


# pylint: disable=protected-access
class SessionFFXTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.ffx.ui.session.py."""

    def setUp(self) -> None:
        super().setUp()

        self.ffx_obj = mock.MagicMock(spec=ffx_transport.FFX)
        self.session_obj = session_using_ffx.SessionUsingFfx(
            device_name="fuchsia-emulator", ffx=self.ffx_obj
        )

    def test_start(self) -> None:
        """Test for Session.start() method."""
        self.session_obj.start()
        self.ffx_obj.run.assert_called_once_with(["session", "start"])

    def test_start_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.start()."""
        self.ffx_obj.run.side_effect = errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.start()

    def test_start_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.start()."""
        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.start()

    def test_add_component(self) -> None:
        """Test for Session.add_component() method."""
        self.session_obj.start()
        self.ffx_obj.reset_mock()

        self.session_obj.add_component(TILE_URL)
        self.ffx_obj.run.assert_called_once_with(["session", "add", TILE_URL])

    def test_add_component_not_started_error(self) -> None:
        """Test for session not started in Session.add_component()."""
        with self.assertRaises(session_errors.SessionError):
            self.session_obj.add_component(TILE_URL)

    def test_add_component_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.add_component()."""
        self.session_obj.start()

        self.ffx_obj.run.side_effect = errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.add_component(TILE_URL)

    def test_add_component_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.add_component()."""
        self.session_obj.start()

        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.add_component(TILE_URL)

    def test_stop(self) -> None:
        """Test for Session.stop() method."""
        self.session_obj.start()
        self.ffx_obj.reset_mock()

        self.session_obj.stop()
        self.ffx_obj.run.assert_called_once_with(["session", "stop"])

    def test_stop_ffx_error(self) -> None:
        """Test for ffx raise ffx error in Session.stop()."""
        self.session_obj.start()

        self.ffx_obj.run.side_effect = errors.FfxCommandError("ffx error")

        with self.assertRaises(session_errors.SessionError):
            self.session_obj.stop()

    def test_stop_timeout_error(self) -> None:
        """Test for ffx raise timeout error in Session.stop()."""
        self.session_obj.start()

        self.ffx_obj.run.side_effect = subprocess.TimeoutExpired("ffx", 1)

        with self.assertRaises(subprocess.TimeoutExpired):
            self.session_obj.stop()


if __name__ == "__main__":
    unittest.main()
