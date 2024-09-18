# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Test to get the list of WLAN PHY devices. This test will only fail if
SL4F or wlandevicemonitor are not running.
"""

import logging

logger = logging.getLogger(__name__)

from antlion import base_test
from antlion.controllers import fuchsia_device
from antlion.controllers.fuchsia_device import FuchsiaDevice
from mobly import asserts, test_runner


class WlanCanaryTest(base_test.AntlionBaseTest):
    def setup_class(self) -> None:
        self.fuchsia_devices: list[FuchsiaDevice] = self.register_controller(
            fuchsia_device
        )

        asserts.abort_class_if(
            len(self.fuchsia_devices) == 0,
            "Requires at least one Fuchsia device",
        )

    def test_example(self) -> None:
        for device in self.fuchsia_devices:
            res = device.sl4f.wlan_lib.get_phy_id_list()
            logger.info(f"List of PHY IDs: {res}")


if __name__ == "__main__":
    test_runner.main()
