#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.bluetooth_avrcp.py."""

import unittest

from honeydew.affordances.fuchsia_controller.bluetooth.profiles import \
    bluetooth_avrcp as fc_bluetooth_avrcp
from honeydew.custom_types import BluetoothAvrcpCommand


class BluetoothFCTests(unittest.TestCase):
    """Unit tests for
    honeydew.affordances.fuchsia_controller.bluetooth_gap.py."""

    def setUp(self) -> None:
        super().setUp()

        self.bluetooth_obj = fc_bluetooth_avrcp.BluetoothAvrcp()

        self.assertIsInstance(
            self.bluetooth_obj, fc_bluetooth_avrcp.BluetoothAvrcp)

    def test_avrcp_init(self) -> None:
        """Test for Bluetooth.init_avrcp() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.init_avrcp(target_id="0")

    def test_list_received_requests(self) -> None:
        """Test for Bluetooth.list_received_requests() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.list_received_requests()

    def test_publish_mock_player(self) -> None:
        """Test for Bluetooth.publish_mock_player() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.publish_mock_player()

    def test_send_avrcp_command(self) -> None:
        """Test for Bluetooth.send_avrcp_command() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.send_avrcp_command(
                command=BluetoothAvrcpCommand.PLAY)

    def test_stop_mock_player(self) -> None:
        """Test for Bluetooth.stop_mock_player() method."""
        with self.assertRaises(NotImplementedError):
            self.bluetooth_obj.stop_mock_player()


if __name__ == "__main__":
    unittest.main()
