# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Screenshot affordance."""

import logging
import os
import time

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.typing import custom_types

_LOGGER = logging.getLogger(__name__)

EXAMPLE_URL = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/flatland-rainbow.cm"
)


class ScreenshotAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Screenshot affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def setup_test(self) -> None:
        super().setup_test()
        self.device.session.stop()
        self.device.session.start()

    def teardown_test(self) -> None:
        self.device.session.stop()
        super().teardown_test()

    def test_take_screenshot(self) -> None:
        # We launch the test app that draws something on the screen.
        # EXAMPLE_URL(flatland-examples) will render colorful background instead
        # of blank screen. It is better for screenshot to verify
        # "It really take a screenshot" instead of "It just give an empty pic".

        _LOGGER.info("Launching %s", EXAMPLE_URL)
        self.device.log_message_to_device(
            f"Launching test app {EXAMPLE_URL}...", custom_types.LEVEL.INFO
        )
        self.device.session.add_component(EXAMPLE_URL)

        # Give the component a chance to load
        # TODO(b/320583170): Can be removed once we have APIs to check for component
        # actually running.
        _LOGGER.info("Waiting for test app to load...")
        time.sleep(10)

        _LOGGER.info("Taking screenshot...")
        self.device.log_message_to_device(
            "Taking screenshot...", custom_types.LEVEL.INFO
        )
        image = self.device.screenshot.take()

        # Save screenshot for debugging
        file_name = f"screenshot_{image.size.width}x{image.size.height}.png"

        _LOGGER.info("Saving screenshot to %s.", file_name)
        image.save(os.path.join(self.test_case_path, file_name))

        asserts.assert_greater(image.size.width, 0)
        asserts.assert_greater(image.size.height, 0)
        asserts.assert_equal(
            image.size.width * image.size.height * 4, len(image.data)
        )

        # Example app render a colorful image with color HSV(?, 75 or 30, 75).
        # Ensure the top left pixel is not black or transparent.
        # TODO(b/320543407): Re-enable the assertion once we get the example app
        # to properly render into scenic. See b/320543407 for details.
        # asserts.assert_not_equal(image.data[0:4], [0x0, 0x0, 0x0, 0xFF])


if __name__ == "__main__":
    test_runner.main()
