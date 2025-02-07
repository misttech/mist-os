# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for PowerSwitchDmc implementation of PowerSwitch interface.

Note - This test relies on DMC which is available in infra host machines and
thus can be run only in infra mode.
"""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

from honeydew.auxiliary_devices.power_switch import (
    power_switch,
    power_switch_using_dmc,
)
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)


class PowerSwitchDmcTest(fuchsia_base_test.FuchsiaBaseTest):
    """Mobly test for PowerSwitchDmc implementation of PowerSwitch interface."""

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        _LOGGER.debug("Instantiating PowerSwitchDmc module")
        self._power_switch: power_switch.PowerSwitch = (
            power_switch_using_dmc.PowerSwitchUsingDmc(
                device_name=self.dut.device_name
            )
        )

    def test_power_switch_dmc(self) -> None:
        """Test case for PowerSwitchDmc.power_off and PowerSwitchDmc.power_on"""
        # Check if device is online before powering off
        self.dut.wait_for_online()

        # power off the device
        self._power_switch.power_off()
        self.dut.wait_for_offline()

        # power on the device
        self._power_switch.power_on()
        self.dut.wait_for_online()
        self.dut.on_device_boot()


if __name__ == "__main__":
    test_runner.main()
