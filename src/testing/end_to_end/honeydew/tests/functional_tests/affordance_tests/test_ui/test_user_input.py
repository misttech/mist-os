#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for UserInput affordance."""

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts
from mobly import test_runner

from honeydew.interfaces.affordances.ui import custom_types
from honeydew.interfaces.device_classes import fuchsia_device

TOUCH_APP = "fuchsia-pkg://fuchsia.com/flatland-examples#meta/" \
            "simplest-app-flatland-session.cm"


class UserInputAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """UserInput affordance tests"""

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
        super().teardown_test()
        self.device.session.stop()

    def test_user_input_tap(self) -> None:
        self.device.session.add_component(TOUCH_APP)

        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.user_input.tap(
                    location=custom_types.Coordinate(x=1, y=2),
                    tap_event_count=1)
            return

        before = self.device.screenshot.take()

        self.device.user_input.tap(
            location=custom_types.Coordinate(x=1, y=2), tap_event_count=1)

        after = self.device.screenshot.take()

        # The app will change the color when a tap is received.
        asserts.assert_not_equal(before.data[0:4], after.data[0:4])


if __name__ == "__main__":
    test_runner.main()
