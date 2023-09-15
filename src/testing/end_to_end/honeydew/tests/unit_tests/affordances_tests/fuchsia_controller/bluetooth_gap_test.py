#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.bluetooth.py."""

import unittest

from honeydew.affordances.fuchsia_controller.bluetooth.profiles import \
    bluetooth_gap as fc_bluetooth_gap
from honeydew.custom_types import BluetoothAcceptPairing
from honeydew.custom_types import BluetoothTransport


class BluetoothFCTests(unittest.TestCase):
    """Unit tests for
    honeydew.affordances.fuchsia_controller.bluetooth_gap.py."""

    def setUp(self) -> None:
        super().setUp()

        self.bluetooth_obj = fc_bluetooth_gap.BluetoothGap()

        self.assertIsInstance(self.bluetooth_obj, fc_bluetooth_gap.BluetoothGap)

    def test_sys_init(self) -> None:
        """Test for Bluetooth.sys_init() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.sys_init()

    def test_accept_pairing(self) -> None:
        """Test for Bluetooth.accept_pairing() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.accept_pairing(
                BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
                BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE)

    def test_connect_device(self) -> None:
        """Test for Bluetooth.connect_device() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.connect_device(
                identifier="0", transport=BluetoothTransport.CLASSIC)

    def test_forget_device(self) -> None:
        """Test for Bluetooth.forget_device() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.forget_device(identifier="0")

    def test_get_active_adapter_address(self) -> None:
        """Test for Bluetooth.get_active_adapter_address() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.get_active_adapter_address()

    def test_get_known_remote_devices(self) -> None:
        """Test for Bluetooth.get_known_remote_devices() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.get_known_remote_devices()

    def test_pair_device(self) -> None:
        """Test for Bluetooth.pair_device() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.pair_device(
                identifier="0", transport=BluetoothTransport.CLASSIC)

    def test_request_discovery(self) -> None:
        """Test for Bluetooth.request_discovery() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.request_discovery(discovery=True)

    def test_set_discoverable(self) -> None:
        """Test for Bluetooth.set_discoverable() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.set_discoverable(discoverable=True)


if __name__ == "__main__":
    unittest.main()
