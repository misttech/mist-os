# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Session affordance."""

import logging
import time

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.affordances.session import errors as session_errors
from honeydew.fuchsia_device import fuchsia_device

_LOGGER = logging.getLogger(__name__)

_TILE_URL = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/flatland-rainbow.cm"
)


class SessionAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Session affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_add_component_without_started_session(self) -> None:
        """Test case for calling session.add_component() without started
        session.

        Ensure it is not a timeout error.
        """

        self.device.session.stop()

        with asserts.assert_raises(session_errors.SessionError):
            self.device.session.add_component(_TILE_URL)

    def test_start_multiple(self) -> None:
        """Test case for session.start() called multiple times."""

        self.device.session.ensure_started()

        # start new session
        self.device.session.start()

        # Give the system a chance to fully start the session before starting
        # the second session.
        _LOGGER.info("Waiting for session to fully start up...")
        time.sleep(10)

        self.device.session.add_component(_TILE_URL)

    def test_stop_stopped_session(self) -> None:
        """Test case for session.stop() called multiple times."""

        self.device.session.ensure_started()
        self.device.session.stop()
        self.device.session.stop()

    def test_restart_session_stopped_session(self) -> None:
        """Test case for session.restart() starting a stopped session."""

        self.device.session.ensure_started()
        started = self.device.session.is_started()
        asserts.assert_true(started, "after session start")

        self.device.session.stop()

        started = self.device.session.is_started()
        asserts.assert_false(started, "after session stop")

        with asserts.assert_raises(session_errors.SessionError):
            # restart when session is stopped will get error: Not Running
            self.device.session.restart()

    def test_restart_session_started_session(self) -> None:
        """Test case for session.restart() restarting a started session."""

        self.device.session.ensure_started()
        self.device.session.restart()

        # Give the system a chance to fully start the session before starting
        # the second session.
        _LOGGER.info("Waiting for session to fully start up...")
        time.sleep(10)

        self.device.session.add_component(_TILE_URL)


if __name__ == "__main__":
    test_runner.main()
