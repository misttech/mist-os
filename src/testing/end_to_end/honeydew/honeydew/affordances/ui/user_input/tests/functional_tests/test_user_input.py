# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for UserInput affordance."""

import os
from typing import Optional

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew import errors
from honeydew.affordances.ui.screenshot import types
from honeydew.affordances.ui.user_input import types as ui_custom_types
from honeydew.fuchsia_device import fuchsia_device
from honeydew.utils import common

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
        self.device.session.ensure_started()
        # Reset screenshot attempt count for each test
        self.screenshot_attempt_count = 0

    def teardown_test(self) -> None:
        self.device.close()
        super().teardown_test()

    def _take_and_save_screenshot(
        self, name_prefix: str, attempt_num: Optional[int] = None
    ) -> types.ScreenshotImage:
        """Takes a screenshot and saves it with a unique name.
        If an image is provided, it just saves it.
        """
        image = self.device.screenshot.take()

        if attempt_num is not None:
            file_name = f"screenshot-{name_prefix}-{attempt_num}.png"
        else:
            file_name = f"screenshot-{name_prefix}.png"

        image.save(os.path.join(self.test_case_path, file_name))
        return image

    def test_user_input_tap(self) -> None:
        self.device.session.add_component(TOUCH_APP)

        # The app will change the color when a tap is received.
        # Ensure the top left pixel changes after tap
        before = self._take_and_save_screenshot("before")

        touch_device = self.device.user_input.create_touch_device()
        touch_device.tap(
            location=ui_custom_types.Coordinate(x=1, y=2), tap_event_count=1
        )

        def pixel_changed_condition() -> bool:
            current_screenshot = self._take_and_save_screenshot(
                f"after", self.screenshot_attempt_count
            )
            self.screenshot_attempt_count += 1
            return before.data[0:4] != current_screenshot.data[0:4]

        try:
            common.wait_for_state(
                state_fn=pixel_changed_condition,
                expected_state=True,
                wait_time=2,  # Time to wait between retries in seconds
            )
        except errors.HoneydewTimeoutError:
            asserts.fail("color did not change after tap within timeout")

    def test_user_input_swipe(self) -> None:
        self.device.session.add_component(TOUCH_APP)

        # The app will change the color when a tap is received.
        # Ensure the top left pixel changes after tap
        before = self._take_and_save_screenshot("before")

        touch_device = self.device.user_input.create_touch_device()

        touch_device.swipe(
            start_location=ui_custom_types.Coordinate(x=1, y=2),
            end_location=ui_custom_types.Coordinate(x=3, y=4),
            move_event_count=2,
            duration_ms=2000,
        )

        def pixel_changed_condition() -> bool:
            current_screenshot = self._take_and_save_screenshot(
                f"after", self.screenshot_attempt_count
            )
            self.screenshot_attempt_count += 1
            return before.data[0:4] != current_screenshot.data[0:4]

        try:
            common.wait_for_state(
                state_fn=pixel_changed_condition,
                expected_state=True,
                wait_time=2,  # Time to wait between retries in seconds
            )
        except errors.HoneydewTimeoutError:
            asserts.fail("color did not change after tap within timeout")

    def test_user_input_press_key(self) -> None:
        self.device.session.add_component(TOUCH_APP)

        # The app will change the color when a key is received.
        # Ensure the top left pixel changes after
        #
        # TODO(b/320543407): Re-enable the assertion once we get the example app
        # to properly render into scenic. See b/320543407 for details.
        # before = self.device.screenshot.take()

        keyboard_device = self.device.user_input.create_keyboard_device()

        keyboard_device.key_press(key_code=0x00070004)  # Key A

        # TODO(b/320543407): Re-enable the assertion once we get the example app
        # to properly render into scenic. See b/320543407 for details.
        # after = self.device.screenshot.take()
        # asserts.assert_not_equal(before.data[0:4], after.data[0:4])


if __name__ == "__main__":
    test_runner.main()
