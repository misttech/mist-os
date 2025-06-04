# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Scenic affordance."""


from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.fuchsia_device import fuchsia_device


class ScenicAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Scenic affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_renderer(self) -> None:
        """Test case for scenic.renderer()"""

        res = self.device.scenic.renderer()
        asserts.assert_in(
            res, ["cpu", "vulkan"], "renderer should be cpu or vulkan"
        )


if __name__ == "__main__":
    test_runner.main()
