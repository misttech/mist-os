# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for UserInput affordance."""

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.interfaces.device_classes import fuchsia_device

TOUCH_APP = (
    "fuchsia-pkg://fuchsia.com/flatland-examples#meta/"
    "simplest-app-flatland-session.cm"
)


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

        # The app will change the color when a tap is received.
        # Ensure the top left pixel changes after tap
        # before = self.device.screenshot.take()

        touch_device = self.device.user_input.create_touch_device()
        touch_device.tap(
            location=ui_custom_types.Coordinate(x=1, y=2), tap_event_count=1
        )

        # TODO(b/320543407): Re-enable the assertion once we get the example app
        # to properly render into scenic. See b/320543407 for details.
        # after = self.device.screenshot.take()
        # asserts.assert_not_equal(before.data[0:4], after.data[0:4])


if __name__ == "__main__":
    test_runner.main()
