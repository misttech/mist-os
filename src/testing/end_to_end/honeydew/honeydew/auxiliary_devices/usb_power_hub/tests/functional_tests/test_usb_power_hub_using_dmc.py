# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for UsbPowerHubUsingDmc implementation of UsbPowerHub interface.

Note - This test relies on DMC which is available in infra host machines and
thus can be run only in infra mode. In order to run this test, Fuchsia Device
needs to be connected to USB power hub in infra and not all fuchsia devices are
connected to a USB power hub.
"""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.auxiliary_devices.usb_power_hub import (
    usb_power_hub,
    usb_power_hub_using_dmc,
)
from honeydew.fuchsia_device import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class UsbPowerHuUsingDmcTest(fuchsia_base_test.FuchsiaBaseTest):
    """Mobly test for UsbPowerDmc implementation of UsbPower interface."""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        _LOGGER.debug("Instantiating UsbPowerDmc module")
        self._usb_power_hub: usb_power_hub.UsbPowerHub = (
            usb_power_hub_using_dmc.UsbPowerHubUsingDmc(
                device_name=self.dut.device_name
            )
        )

    def test_usb_power_hub_using_dmc(self) -> None:
        """Test case for UsbPowerHubUsingDmc.power_off and UsbPowerHubUsingDmc.power_on"""
        # Check if device is online before powering off
        self.dut.wait_for_online()

        # power off the usb connection
        self._usb_power_hub.power_off()
        # self.dut.wait_for_offline()  # TODO(https://fxbug.dev/431799077): Reenable when fixed.

        # power on the usb connection
        self._usb_power_hub.power_on()
        self.dut.wait_for_online()
        self.dut.on_device_boot()


if __name__ == "__main__":
    test_runner.main()
