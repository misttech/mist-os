#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.bluetooth_default.py."""
import unittest
from unittest import mock
from honeydew.affordances import bluetooth_default
from honeydew.interfaces.transports import sl4f


# pylint: disable=protected-access
class BluetoothDefaultTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.bluetooth_default.py."""

    def setUp(self) -> None:
        super().setUp()
        self.sl4f_obj = mock.MagicMock(spec=sl4f.SL4F)
        self.bluetooth_obj = bluetooth_default.BluetoothDefault(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj)

    def test_request_discovery(self):
        """Test for request_discovery."""
        self.bluetooth_obj.request_discovery(True)
        self.sl4f_obj.send_sl4f_command.assert_called_once_with(
            method=bluetooth_default._SL4F_METHODS["BluetoothRequestDiscovery"],
            params={"discovery": True})
