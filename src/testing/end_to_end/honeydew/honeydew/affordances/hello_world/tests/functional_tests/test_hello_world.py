# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for HelloWorld affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.fuchsia_device import fuchsia_device

LOGGER: logging.Logger = logging.getLogger(__name__)


class HelloWorldAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """HelloWorld affordance tests."""

    def setup_class(self) -> None:
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_hello_world_greeting(self) -> None:
        """Test case for HelloWorld.greeting() method"""
        asserts.assert_equal(
            self.dut.hello_world.greeting(), f"Hello, {self.dut.device_name}!"
        )


if __name__ == "__main__":
    test_runner.main()
