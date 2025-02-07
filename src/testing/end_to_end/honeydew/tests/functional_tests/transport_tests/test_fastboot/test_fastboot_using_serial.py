# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Fastboot transport."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew import errors
from honeydew.auxiliary_devices.power_switch import (
    power_switch as power_switch_interface,
)
from honeydew.auxiliary_devices.power_switch import power_switch_using_dmc
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.interfaces.transports import serial as serial_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FastbootUsingSerialTests(fuchsia_base_test.FuchsiaBaseTest):
    """Test class to test rebooting the device into Fastboot mode using serial transport.

    Note: This test case can only be run in infra as it uses below which are available only in infra:
    * PowerSwitch auxiliary device implementation using DMC
    * Serial transport implementation using unix socket
    """

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
            * Calls some Fastboot transport method to initialize Fastboot
              transport (as it may involve device reboots)
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

        # Calling some fastboot method here, so that Fastboot __init__ gets called which will
        # retrieve the fastboot node-id.
        # Reason for doing this is, if DUT uses TCP based fastboot connection then retrieving the
        # fastboot node-id involves:
        #   * rebooting the device into fastboot mode
        #   * retrieve the fastboot node-id
        #   * reboot back to fuchsia mode
        # So to avoid all these additional steps in actual test case, we are explicitly
        # instantiating fastboot transport in setup_class
        self._fastboot_node_id: str = self.device.fastboot.node_id

    def teardown_test(self) -> None:
        """teardown_test is called once after running each test.

        It does the following things:
            * Ensures device is in fuchsia mode.
        """
        super().teardown_test()
        if self.device.fastboot.is_in_fastboot_mode():
            _LOGGER.warning(
                "%s is in fastboot mode which is not expected. "
                "Rebooting to fuchsia mode",
                self.device.device_name,
            )
            self.device.fastboot.boot_to_fuchsia_mode()

    def test_fastboot_using_serial(self) -> None:
        """Test case that puts the device in fastboot mode using serial, runs
        a command in fastboot mode and reboots the device back to fuchsia mode.
        """
        power_switch: power_switch_interface.PowerSwitch
        serial: serial_transport.Serial

        try:
            power_switch = power_switch_using_dmc.PowerSwitchUsingDmc(
                device_name=self.device.device_name
            )
        except power_switch_using_dmc.PowerSwitchDmcError:
            asserts.fail(
                "PowerSwitchDmc is not available. This test can't be run."
            )

        try:
            serial = self.device.serial
        except errors.FuchsiaDeviceError:
            asserts.fail(
                "Access to device serial port via unix socket is not available. "
                "This test can't be run."
            )

        self.device.fastboot.boot_to_fastboot_mode(
            use_serial=True,
            serial_transport=serial,
            power_switch=power_switch,
        )

        self.device.fastboot.run(cmd=["getvar", "hw-revision"])

        self.device.fastboot.boot_to_fuchsia_mode()


if __name__ == "__main__":
    test_runner.main()
