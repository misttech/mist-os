# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Test to get the list of WLAN PHY devices. This test will only fail if
wlandevicemonitor is not running.
"""

import logging

logger = logging.getLogger(__name__)

import fidl_fuchsia_wlan_device_service as fidl_wlan_device_service
from antlion.controllers import fuchsia_device
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod
from honeydew.typing.custom_types import FidlEndpoint
from mobly import base_test, test_runner
from mobly.asserts import abort_class_if


class WlanCanaryTest(AsyncAdapter, base_test.BaseTestClass):
    def setup_class(self) -> None:
        fuchsia_devices = self.register_controller(fuchsia_device)

        abort_class_if(
            len(fuchsia_devices) != 1, "Requires exactly one Fuchsia device"
        )
        self.fuchsia_device = fuchsia_devices[0]
        abort_class_if(
            not hasattr(self.fuchsia_device, "honeydew_fd")
            or self.fuchsia_device.honeydew_fd is None,
            "Requires a Honeydew-enabled FuchsiaDevice",
        )

        self.wlan_device_monitor_proxy = fidl_wlan_device_service.DeviceMonitorClient(
            self.fuchsia_device.honeydew_fd.fuchsia_controller.connect_device_proxy(
                FidlEndpoint(
                    "core/wlandevicemonitor",
                    "fuchsia.wlan.device.service.DeviceMonitor",
                )
            )
        )

    @asyncmethod
    async def test_wlandevicemonitor_is_responsive(self) -> None:
        phy_list = (await self.wlan_device_monitor_proxy.list_phys()).phy_list
        logger.info(f"List of PHY IDs: {phy_list}")


if __name__ == "__main__":
    test_runner.main()
