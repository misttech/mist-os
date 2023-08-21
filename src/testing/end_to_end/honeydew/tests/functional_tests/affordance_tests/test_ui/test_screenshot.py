#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Screenshot affordance."""

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts

from honeydew.interfaces.device_classes import fuchsia_device

EXAMPLE_URL = "fuchsia-pkg://fuchsia.com/flatland-examples#meta/" \
              "flatland-view-provider.cm"


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
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.screenshot.take()
            return

        self.device.session.add_component(EXAMPLE_URL)

        image = self.device.screenshot.take()
        asserts.assert_greater(image.size.width, 0)
        asserts.assert_greater(image.size.height, 0)
        asserts.assert_equal(
            image.size.width * image.size.height * 4, len(image.data))

        # Example app render a colorful image with color HSV(?, 75 or 30, 75).
        # Ensure the top left pixel is not BGRA(0, 0, 0, 255).
        asserts.assert_equal(image.data[0:4], [0x0, 0x0, 0x0, 0xff])
