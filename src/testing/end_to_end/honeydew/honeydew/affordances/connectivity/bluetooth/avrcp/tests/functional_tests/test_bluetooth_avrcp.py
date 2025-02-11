# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Bluetooth Avrcp affordance."""

import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.affordances.connectivity.bluetooth.utils import (
    types as bluetooth_types,
)
from honeydew.errors import Sl4fError
from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)

BluetoothAvrcpCommand = bluetooth_types.BluetoothAvrcpCommand


class BluetoothAvrcpAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """BluetoothAvrcp affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_avrcp_init(self) -> None:
        """Test for Bluetooth.avrcp_init() method."""
        self.device.bluetooth_avrcp.init_avrcp(target_id="0")

    def test_list_received_requests(self) -> None:
        """Test for Bluetooth.list_received_requests() method."""
        res = self.device.bluetooth_avrcp.list_received_requests()
        assert len(res) == 0

    def test_publish_mock_player(self) -> None:
        """Test for Bluetooth.publish_mock_player() method."""
        self.device.bluetooth_avrcp.publish_mock_player()

    def test_send_avrcp_command(self) -> None:
        """Test for Bluetooth.send_avrcp_command() method."""
        # Currently fails sending commands since we only test single device
        with asserts.assert_raises(Sl4fError):
            self.device.bluetooth_avrcp.send_avrcp_command(
                BluetoothAvrcpCommand.PLAY
            )

    def test_stop_mock_player(self) -> None:
        """Test for Bluetooth.stop_mock_player() method"""
        self.device.bluetooth_avrcp.stop_mock_player()


if __name__ == "__main__":
    test_runner.main()
