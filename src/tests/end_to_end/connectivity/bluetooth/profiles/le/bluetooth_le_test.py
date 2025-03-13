#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Sample test that demonstrates the usage of 2 Fuchsia devices in one test"""
import logging
import re
import time
from typing import Any, List

from fuchsia_base_test import fuchsia_base_test
from honeydew.affordances.connectivity.bluetooth.utils.types import (
    BluetoothAcceptPairing,
    BluetoothLEAppearance,
)
from mobly import test_runner

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
        self.central = self.fuchsia_devices[0]
        self.peripheral = self.fuchsia_devices[1]

    def _test_logic(self, iteration: int) -> None:
        """Minimal Viable Code for BLE testing
        1. Start Pairing Delegate Server
        2. Advertise from Peripheral end
        3. Call le_scan from Central
        4. Call connect from the central
        5. Call get_known_remote_devices from central to verify "connect: True"
        """
        self.peripheral.bluetooth_le.accept_pairing(
            input_mode=BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
            output_mode=BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE,
        )
        self._set_discoverability_on()
        _LOGGER.info(
            "Starting the Bluetooth Sample test iteration# %s", iteration
        )
        _LOGGER.info("Initializing Bluetooth and setting discoverability")
        _LOGGER.info(self.peripheral.device_name)
        self.peripheral.bluetooth_le.advertise(
            appearance=BluetoothLEAppearance.GLUCOSE_MONITOR,
            name=self.peripheral.device_name,
        )
        known_device = self.central.bluetooth_le.scan()
        _LOGGER.info(known_device)
        _LOGGER.info("Finished scanning, now going to connect.")
        for k in known_device.keys():
            self.central.bluetooth_le.connect(k)
            _LOGGER.info("Finished attempting to connect")
            self.peripheral.bluetooth_le.run_advertise_connection()
        time.sleep(5)
        print(self.central.bluetooth_le.get_known_remote_devices())

    def teardown_test(self) -> None:
        """Teardown test will turn off discoverability for all the devices."""
        _LOGGER.info("Forgetting all connected devices on Central/Peripheral.")
        self.central.bluetooth_le.reset_state()
        self.peripheral.bluetooth_le.reset_state()
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

    def _verify_peripheral_is_discovered(
        self, data: dict[str, dict[str, Any]], reverse_hex_address: List[int]
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
        _LOGGER.info("TRYING TO SET DISCOVERABLE")
        self.central.bluetooth_le.request_discovery(True)
        self.central.bluetooth_le.set_discoverable(True)
        self.peripheral.bluetooth_le.set_discoverable(True)

    def _forget_all_bt_devices(self, device: Any, id: Any = None) -> None:
        """Unpairs and deletes any BT peer pairing data from the device."""
        if id is None:
            data = device.bluetooth_le.get_known_remote_devices()
            _LOGGER.info(data)
            for device_id in data.keys():
                device.bluetooth_le.forget_device(
                    identifier=data[device_id]["id"]
                )
        else:
            device.bluetooth_le.forget_device(identifier=id)


if __name__ == "__main__":
    test_runner.main()
