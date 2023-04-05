#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.bluetooth_default.py."""

import unittest
from typing import Any, Dict
from unittest import mock

from honeydew.affordances import bluetooth_default
from honeydew.interfaces.transports import sl4f
from parameterized import parameterized


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class BluetoothDefaultTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.bluetooth_default.py."""

    def setUp(self) -> None:
        super().setUp()
        self.sl4f_obj = mock.MagicMock(spec=sl4f.SL4F)
        self.bluetooth_obj = bluetooth_default.BluetoothDefault(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj)
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=bluetooth_default._SL4F_METHODS["BluetoothInitSys"])
        self.sl4f_obj.reset_mock()

    def test_sys_init(self) -> None:
        """Test for BluetoothDefault.sys_init() method."""
        self.bluetooth_obj.sys_init()
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=bluetooth_default._SL4F_METHODS["BluetoothInitSys"])

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
        """Test for BluetoothDefault.request_discovery() method."""
        self.bluetooth_obj.request_discovery(
            discovery=parameterized_dict["discovery"])
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=bluetooth_default._SL4F_METHODS["BluetoothRequestDiscovery"],
            params={"discovery": parameterized_dict["discovery"]})


if __name__ == "__main__":
    unittest.main()
