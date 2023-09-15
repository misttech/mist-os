#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.bluetooth.py."""

from typing import Any, Dict
import unittest
from unittest import mock

from parameterized import parameterized

from honeydew.affordances.sl4f.bluetooth.profiles import \
    bluetooth_gap as sl4f_bluetooth_gap
from honeydew.custom_types import BluetoothAcceptPairing
from honeydew.custom_types import BluetoothTransport
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import sl4f as sl4f_transport

_SAMPLE_ADDRESS_OUTPUT: Dict[str, Any] = {
    "id": "",
    "result": "[address (public) 20:1F:3B:62:E9:D2]",
    "error": None
}

_SAMPLE_KNOWN_DEVICES_OUTPUT: Dict[str, Any] = {
    "id": "",
    "result":
        {
            "16085008211800713200":
                {
                    "address": [88, 111, 107, 249, 15, 248],
                    "appearance": None,
                    "bonded": False,
                    "connected": False,
                    "device_class": 2097408,
                    "id": "16085008211800713200",
                    "name": "fuchsia-f80f-f96b-6f59",
                    "rssi": -17,
                    "services": None,
                    "technology": 2,
                    "tx_power": None
                }
        },
    "error": None
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class BluetoothGapSL4FTests(unittest.TestCase):
    """Unit tests for
    honeydew.affordances.sl4f.bluetooth.bluetooth_gap.py.
    """

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice)

        self.bluetooth_obj = sl4f_bluetooth_gap.BluetoothGap(
            device_name="fuchsia-emulator",
            sl4f=self.sl4f_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

        self.sl4f_obj.run.assert_called()
        self.sl4f_obj.reset_mock()

    def test_sys_init(self) -> None:
        """Test for Bluetooth.sys_init() method."""
        self.bluetooth_obj.sys_init()

        self.sl4f_obj.run.assert_called()

    def test_accept_pairing(self) -> None:
        """Test for Bluetooth.accept_pairing() method."""
        self.bluetooth_obj.accept_pairing(
            BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
            BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE)

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "pair_classic",
                    "transport": BluetoothTransport.CLASSIC
                },),
            (
                {
                    "label": "pair_low_energy",
                    "transport": BluetoothTransport.LOW_ENERGY
                },),
        ],
        name_func=_custom_test_name_func)
    def test_connect_device(self, parameterized_dict) -> None:
        """Test for Bluetooth.connect_device() method."""
        dummy_identifier = "0"
        self.bluetooth_obj.connect_device(
            identifier=dummy_identifier,
            transport=parameterized_dict["transport"])

        self.sl4f_obj.run.assert_called()

    def test_forget_device(self) -> None:
        """Test for Bluetooth.forget_device() method."""
        dummy_identifier = "0"
        self.bluetooth_obj.forget_device(dummy_identifier)

        self.sl4f_obj.run.assert_called()

    def test_get_active_adapter_address(self) -> None:
        """Test for Bluetooth.get_active_adapter_address() method."""
        self.sl4f_obj.run.return_value = _SAMPLE_ADDRESS_OUTPUT
        res = self.bluetooth_obj.get_active_adapter_address()
        self.sl4f_obj.run.assert_called()
        assert res == "20:1F:3B:62:E9:D2"

    def test_get_known_remote_devices(self) -> None:
        """Test for Bluetooth.get_known_remote_devices() method."""
        self.sl4f_obj.run.return_value = _SAMPLE_KNOWN_DEVICES_OUTPUT
        res = self.bluetooth_obj.get_known_remote_devices()
        self.sl4f_obj.run.assert_called()
        assert res["16085008211800713200"]["id"] == "16085008211800713200"

    @parameterized.expand(
        [
            (
                {
                    "label": "pair_classic",
                    "transport": BluetoothTransport.CLASSIC
                },),
            (
                {
                    "label": "pair_low_energy",
                    "transport": BluetoothTransport.LOW_ENERGY
                },),
        ],
        name_func=_custom_test_name_func)
    def test_pair_device(self, parameterized_dict) -> None:
        """Test for Bluetooth.pair_device() method."""
        dummy_identifier = "0"
        self.bluetooth_obj.pair_device(
            identifier=dummy_identifier,
            transport=parameterized_dict["transport"])

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            ({
                "label": "discovery_true",
                "discovery": True
            },),
            ({
                "label": "discovery_false",
                "discovery": False
            },),
        ],
        name_func=_custom_test_name_func)
    def test_request_discovery(self, parameterized_dict) -> None:
        """Test for Bluetooth.request_discovery() method."""
        self.bluetooth_obj.request_discovery(
            discovery=parameterized_dict["discovery"])

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            ({
                "label": "set_discoverable_true",
                "discoverable": True
            },),
            ({
                "label": "set_discoverable_false",
                "discoverable": False
            },),
        ],
        name_func=_custom_test_name_func)
    def test_set_discoverable(self, parameterized_dict) -> None:
        """Test for Bluetooth.set_discoverable() method."""
        self.bluetooth_obj.set_discoverable(
            discoverable=parameterized_dict["discoverable"])
        self.sl4f_obj.run.assert_called()


if __name__ == "__main__":
    unittest.main()
