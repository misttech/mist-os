# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Session affordance."""

import logging
import time

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew import errors
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER = logging.getLogger(__name__)

TILE_URL = (
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

    def setup_test(self) -> None:
        super().setup_test()

        # stop session for a clean state.
        self.device.session.stop()

    def teardown_test(self) -> None:
        self.device.session.stop()
        super().teardown_test()

    def test_add_component(self) -> None:
        """Test case for session.add_component()"""

        self.device.session.start()
        self.device.session.add_component(TILE_URL)

    def test_add_component_without_started_session(self) -> None:
        """Test case for calling session.add_component() without started
        session.

        Ensure it is not a timeout error.
        """

        with asserts.assert_raises(errors.SessionError):
            self.device.session.add_component(TILE_URL)

    def test_add_component_wrong_url(self) -> None:
        """Test case for session.add_component() with wrong url."""

        self.device.session.start()

        wrong_url = "INVALID_URL"

        with asserts.assert_raises(errors.SessionError):
            self.device.session.add_component(wrong_url)

    def test_add_component_twice(self) -> None:
        """Test case for session.add_component() called twice."""

        _LOGGER.info("Waiting for Starnix kernel to shut down")
        while True:
            components: str = self.device.ffx.run(["component", "list"])
            if components.find("starnix_runner/kernels") == -1:
                _LOGGER.info("Starnix kernel has shut down")
                break

        self.device.session.start()
        self.device.session.add_component(TILE_URL)
        self.device.session.add_component(TILE_URL)

    def test_start_multiple(self) -> None:
        """Test case for session.start() called multiple times."""

        self.device.session.start()

        # Give the system a chance to fully start the session before starting
        # the second session.
        _LOGGER.info("Waiting for session to fully start up...")
        time.sleep(10)

        self.device.session.start()
        self.device.session.add_component(TILE_URL)

    def test_stop_stopped_session(self) -> None:
        """Test case for session.stop() called multiple times."""

        self.device.session.start()
        self.device.session.stop()
        self.device.session.stop()


if __name__ == "__main__":
    test_runner.main()
