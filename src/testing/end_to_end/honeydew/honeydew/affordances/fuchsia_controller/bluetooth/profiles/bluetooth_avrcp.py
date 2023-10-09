#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia-Controller implementation for Bluetooth AVRCP Profile affordance."""

from honeydew.affordances.fuchsia_controller.bluetooth import bluetooth_common
from honeydew.interfaces.affordances.bluetooth.profiles import bluetooth_avrcp
from honeydew.typing import bluetooth

BluetoothAvrcpCommand = bluetooth.BluetoothAvrcpCommand


class BluetoothAvrcp(
    bluetooth_common.BluetoothCommon, bluetooth_avrcp.BluetoothAvrcp
):
    """Fuchsia-Controller for BluetoothAvrcp Profile affordance."""

    # List all the public methods in alphabetical order
    def init_avrcp(self, target_id: str) -> None:
        """Initialize AVRCP service from the sink device.

        Args:
            target_id: id of source device to start AVRCP
        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def list_received_requests(self) -> list:
        """List received requests received from source device.

        Returns:
            A list of the most recent commands received, where the last
            element in the list is the most recent command received.
        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def publish_mock_player(self) -> None:
        """Publish the media session mock player.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def send_avrcp_command(self, command: BluetoothAvrcpCommand) -> None:
        """Send Avrcp command from the sink device.

        Args:
            command: the command to send to the AVRCP service.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def stop_mock_player(self) -> None:
        """Stop the media session mock player.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError
