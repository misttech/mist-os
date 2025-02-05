# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Bluetooth LE Profile affordance."""

import abc
from typing import Any

import fidl.fuchsia_bluetooth as f_bt

from honeydew.affordances.connectivity.bluetooth.bluetooth_common import (
    bluetooth_common,
)
from honeydew.affordances.connectivity.bluetooth.utils import types as bt_types


class LE(bluetooth_common.BluetoothCommon):
    """Abstract base class for Bluetooth LE Profile affordance."""

    # TODO(b/352584355): Add functional tests for BLE affordance
    # List all the public methods
    @abc.abstractmethod
    def advertise(
        self, appearance: bt_types.BluetoothLEAppearance, name: str
    ) -> None:
        """Advertise the peripheral.

        Args:
            appearance: Peripheral device appearance.
            name: Peripheral device name.

        Raises:
            BluetoothError: If the peripheral fails to advertise.
        """

    @abc.abstractmethod
    def connect(self, identifier: Any) -> None:
        """Initiate connection from the central device to peripheral.

        Args:
            identifier: The identifier of the peripheral.

        Raises:
            BluetoothError: If the peripheral fails to connect to central device.
        """

    @abc.abstractmethod
    def connect_to_service(self, handle: int) -> None:
        """Connect to available Gatt services on the central device.

        Args:
            handle: The handle of the service.

        Raises:
            BluetoothError: If the central device fails to connect to Gatt service.
        """

    @abc.abstractmethod
    def discover_characteristics(
        self,
    ) -> dict[int, dict[str, int | list[int] | None]]:
        """Discover characteristics of a connected Gatt Service.

        Returns:
            The available characteristics of a connected Gatt Service.
        """

    @abc.abstractmethod
    def list_gatt_services(self) -> dict[int, dict[str, Any]]:
        """List the Gatt Services found on the connected peripheral.

        Raises:
            BluetoothError: If the device fails to complete the FIDL request.
        """

    @abc.abstractmethod
    def init_le_sys(self) -> None:
        """Initializes ble stack.

        Note: This method is called automatically:
            1. During this class initialization
            2. After the device reboot

        Raises:
            errors.BluetoothStateError: On failure.
        """

    @abc.abstractmethod
    def publish_service(self) -> f_bt.Uuid:
        """Publish the Gatt service from the peripheral.

        Returns:
            The UUID of the service.

        Raises:
            BluetoothError: If the peripheral fails to publish the Gatt service.
        """

    @abc.abstractmethod
    def read_characteristic(self, handle: int) -> None:
        """Read characteristic of the Gatt service.

        Args:
            handle: The handle of the service.

        Returns:
            A characteristic of the Gatt service and its properties

        Raises:
            BluetoothError: If the peripheral fails to read the characteristic.
        """

    @abc.abstractmethod
    def request_gatt_client(self) -> None:
        """Request the Gatt Client.

        Raises:
            BluetoothError: If the peripheral fails to request the Gatt client.
        """

    @abc.abstractmethod
    def stop_advertise(self) -> None:
        """Stop advertising the peripheral."""

    @abc.abstractmethod
    def scan(self) -> dict[str, Any]:
        """Perform an LE scan on central device.

        Returns:
            The scan result.

        Raises:
            BluetoothError: If the central device fails to complete the scan FIDL.
        """
