#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Sample test that demonstrates the usage of 2 Fuchsia devices in one test"""
import logging
import re
import time
from typing import Any

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class MultipleFuchsiaDevicesNotFound(Exception):
    """When there are less than two Fuchsia devices available."""


class MultiDeviceSampleTest(fuchsia_base_test.FuchsiaBaseTest):
    """Sample test that uses multiple Fuchsia devices"""

    def pre_run(self) -> None:
        """Mobly method used to generate the test cases at run time."""
        test_arg_tuple_list: list[tuple[int]] = []

        for iteration in range(1, int(self.user_params["num_iterations"]) + 1):
            test_arg_tuple_list.append((iteration,))

        self.generate_tests(
            test_logic=self._test_logic,
            name_func=self._name_func,
            arg_sets=test_arg_tuple_list,
        )

    def _name_func(self, iteration: int) -> str:
        return f"test_bluetooth_sample_test_{iteration}"

    def setup_class(self) -> None:
        """Initialize all DUTs."""
        super().setup_class()
        if len(self.fuchsia_devices) < 2:
            raise MultipleFuchsiaDevicesNotFound(
                "Two FuchsiaDevices are" "required to run BluetoothSampleTest"
            )
        self.initiator = self.fuchsia_devices[0]
        self.receiver = self.fuchsia_devices[1]

    def _test_logic(self, iteration: int) -> None:
        """Test Logic for Bluetooth Sample Test
        1. Turn on BT discoverability on both devices
        2. Retrieve the receiver's BT address
        3. Receive all broadcasting BT devices on initiator side.
        4. Check that the receiver is broadcasting to initiator.
        """

        _LOGGER.info(
            "Starting the Bluetooth Sample test iteration# %s", iteration
        )
        _LOGGER.info("Initializing Bluetooth and setting discoverability")
        self._set_discoverability_on()

        address = self.receiver.bluetooth_gap.get_active_adapter_address()

        _LOGGER.info(
            "Sleep for 5 seconds to wait for dut to listen for receiever"
        )
        time.sleep(5)
        known_device = self.initiator.bluetooth_gap.get_known_remote_devices()
        receiver_address_converted = self._sl4f_bt_mac_address(
            mac_address=address
        )
        asserts.assert_true(
            self._verify_receiver_is_discovered(
                data=known_device,
                reverse_hex_address=receiver_address_converted,
            ),
            msg="Receiver was not discovered.",
        )

    def teardown_test(self) -> None:
        """Teardown test will turn off discoverability for all the devices."""
        _LOGGER.info("Turning off discoverability on all devices")
        self.initiator.bluetooth_gap.set_discoverable(False)
        self.receiver.bluetooth_gap.set_discoverable(False)

        return super().teardown_class()

    def _sl4f_bt_mac_address(self, mac_address: str) -> list[int]:
        """Converts BT mac addresses to reversed BT byte lists.
        Args:
            mac_address: mac address of device
            Ex. "00:11:22:33:FF:EE"

        Returns:
            Mac address to reverse hex in form of a list
            Ex. [88, 111, 107, 249, 15, 248]
        """
        if ":" in mac_address:
            return self._convert_reverse_hex(mac_address.split(":"))
        return self._convert_reverse_hex(re.findall("..", mac_address))

    def _convert_reverse_hex(self, address: list[str]) -> list[int]:
        """Reverses ASCII mac address to 64-bit byte lists.
        Args:
            address: Mac address of device
            Ex. "00112233FFEE"

        Returns:
            Mac address to reverse hex in form of a list
            Ex. [88, 111, 107, 249, 15, 248]
        """
        res = []
        for x in reversed(address):
            res.append(int(x, 16))
        return res

    def _verify_receiver_is_discovered(
        self, data: dict[str, dict[str, Any]], reverse_hex_address: list[int]
    ) -> bool:
        """Verify if we have seen the reciever via the Bluetooth data
        Args:
            data: All known discoverable devices via Bluetooth
                and information
            reverse_hex_address: BT address to look for
        Returns:
            True: If we found the broadcasting bluetooth address
        """
        for value in data.values():
            if value["address"] == reverse_hex_address:
                return True
        return False

    def _set_discoverability_on(self) -> None:
        """Turns on discoverability for the devices."""
        self.initiator.bluetooth_gap.request_discovery(True)
        self.initiator.bluetooth_gap.set_discoverable(True)
        self.receiver.bluetooth_gap.request_discovery(True)
        self.receiver.bluetooth_gap.set_discoverable(True)


if __name__ == "__main__":
    test_runner.main()
