#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.bluetooth.py."""

import unittest

from honeydew.affordances.fuchsia_controller import bluetooth as fc_bluetooth


class BluetoothFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.bluetooth.py."""

    def setUp(self) -> None:
        super().setUp()

        self.bluetooth_obj = fc_bluetooth.Bluetooth()

        self.assertIsInstance(self.bluetooth_obj, fc_bluetooth.Bluetooth)

    def test_sys_init(self) -> None:
        """Test for Bluetooth.sys_init() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.sys_init()

    def test_request_discovery(self) -> None:
        """Test for Bluetooth.request_discovery() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.request_discovery(discovery=True)


if __name__ == "__main__":
    unittest.main()
