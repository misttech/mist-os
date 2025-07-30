# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Usb Power Hub test."""

import logging

from fuchsia_base_test import fuchsia_base_test
from honeydew.auxiliary_devices.usb_power_hub import usb_power_hub
from honeydew.fuchsia_device import fuchsia_device
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class UsbDisconnectTest(fuchsia_base_test.FuchsiaBaseTest):
    """Mobly test for testing the UsbPowerHub that works locally or in infra.

     Attributes:
        dut: FuchsiaDevice object.

    Required Mobly Test Params:
        num_usb_disconnects (int): Number of times usb_disconnect test need to be
            executed.
    """

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: list[tuple[int]] = []

        for iteration in range(
            1, int(self.user_params["num_usb_disconnects"]) + 1
        ):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def setup_class(self) -> None:
        """setup_class is called once before running tests."""
        super().setup_class()
        self.dut: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]
        self._usb_power_hub: usb_power_hub.UsbPowerHub
        self._usb_port: int | None
        (self._usb_power_hub, self._usb_port) = self._lookup_usb_power_hub(
            self.dut
        )

    def _test_logic(self, iteration: int) -> None:
        """Test case logic that disconnects the USB from a fuchsia device."""
        _LOGGER.info(
            "Starting the Usb Disconnect test iteration# %s", iteration
        )
        self._usb_power_hub.power_off(port=self._usb_port)
        # self.dut.wait_for_offline()  # TODO(https://fxbug.dev/431799077): Reenable when fixed.
        self._usb_power_hub.power_on(port=self._usb_port)
        self.dut.wait_for_online()
        self.dut.on_device_boot()
        _LOGGER.info(
            "Successfully ended the Usb Disconnect test iteration# %s",
            iteration,
        )

    def _name_func(self, iteration: int) -> str:
        """This function generates the names of each test case based on each
        argument set.

        The name function should have the same signature as the actual test
        logic function.

        Returns:
            Test case name
        """
        return f"test_usb_disconnect_{iteration}"


if __name__ == "__main__":
    test_runner.main()
