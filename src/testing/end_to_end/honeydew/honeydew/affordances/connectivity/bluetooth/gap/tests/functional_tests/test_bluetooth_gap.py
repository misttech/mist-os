# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Bluetooth affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.affordances.connectivity.bluetooth.utils import types as bt_types
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)

BluetoothAcceptPairing = bt_types.BluetoothAcceptPairing
BluetoothConnectionType = bt_types.BluetoothConnectionType


class BluetoothGapAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """BluetoothGap affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_bluetooth_accept_pairing(self) -> None:
        """Test case for bluetooth.accept_pairing()"""

        input_mode = BluetoothAcceptPairing.DEFAULT_INPUT_MODE
        output_mode = BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE

        self.device.bluetooth_gap.accept_pairing(input_mode, output_mode)

    def test_bluetooth_connect_device(self) -> None:
        """Test case for bluetooth.connect_device()"""

        identifier = 000000000
        transport = BluetoothConnectionType.CLASSIC
        self.device.bluetooth_gap.connect_device(identifier, transport)

    def test_bluetooth_forget_device(self) -> None:
        """Test case for bluetooth.forget_device()"""

        identifier = 000000000

        self.device.bluetooth_gap.forget_device(identifier)

    def test_get_active_adapter_address(self) -> None:
        """Test case for bluetooth.get_active_adapter_address()"""
        self.device.bluetooth_gap.get_active_adapter_address()

    def test_bluetooth_get_connected_devices(self) -> None:
        """Test case for bluetooth.get_connected_devices()"""

        res = self.device.bluetooth_gap.get_connected_devices()
        asserts.assert_equal(res, [])

    def test_bluetooth_get_known_remote_devices(self) -> None:
        """Test case for bluetooth.get_known_remote_devices()"""

        self.device.bluetooth_gap.get_known_remote_devices()

    def test_bluetooth_pair_device(self) -> None:
        """Test case for bluetooth.pair_device()"""

        identifier = 000000000
        transport = BluetoothConnectionType.CLASSIC

        self.device.bluetooth_gap.pair_device(identifier, transport)

    def test_bluetooth_request_discovery(self) -> None:
        """Test case for bluetooth.request_discovery()"""

        self.device.bluetooth_gap.request_discovery(True)

    def test_bluetooth_set_discoverable(self) -> None:
        """Test case for bluetooth_gap.set_discoverable()"""

        self.device.bluetooth_gap.set_discoverable(True)


if __name__ == "__main__":
    test_runner.main()
