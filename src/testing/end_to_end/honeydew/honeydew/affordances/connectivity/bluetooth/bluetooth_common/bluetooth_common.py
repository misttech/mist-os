# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Common bluetooth affordance that other bluetooth profiles/affordances
will depend on."""

import abc
from typing import Any, Optional

from honeydew.affordances.connectivity.bluetooth.utils import (
    types as bluetooth_types,
)


class BluetoothCommon(abc.ABC):
    """Abstract base class for Common bluetooth affordance that other bluetooth profiles/affordances
    will depend on."""

    @abc.abstractmethod
    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def accept_pairing(
        self,
        input_mode: bluetooth_types.BluetoothAcceptPairing,
        output_mode: bluetooth_types.BluetoothAcceptPairing,
        timeout_sec: Optional[int | None],
    ) -> None:
        """Sets device to accept Bluetooth pairing.

        Args:
            input: input mode of device.
            output: output mode of device.
            timeout_sec: timeout duration in seconds

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def connect_device(
        self,
        identifier: str,
        connection_type: bluetooth_types.BluetoothConnectionType,
        timeout_sec: Optional[int | None],
    ) -> None:
        """Connect device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            connection_type: type of bluetooth connection.
            timeout_sec: timeout duration in seconds

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def forget_device(
        self, identifier: str, timeout_sec: Optional[int | None]
    ) -> None:
        """Forget device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            timeout_sec: timeout duration in seconds

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def get_active_adapter_address(
        self, timeout_sec: Optional[int | None]
    ) -> str:
        """Retrieves the device's active BT adapter address.

        Args:
            timeout_sec: timeout duration in seconds

        Returns:
            The mac address of the active adapter.

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def get_connected_devices(self) -> list[str]:
        """Retrieves all connected remote devices.

        Returns:
            A list of all connected devices by identifier.

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def get_known_remote_devices(
        self, timeout_sec: Optional[int | None] = 0
    ) -> dict[str, Any]:
        """Retrieves all known remote devices received by device.

        Args:
            timeout_sec: timeout duration in seconds

        Returns:
            A dict of all known remote devices.

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def pair_device(
        self,
        identifier: str,
        connection_type: bluetooth_types.BluetoothConnectionType,
        timeout_sec: Optional[int | None],
    ) -> None:
        """Pair device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            connection_type: type of bluetooth connection.
            timeout_sec: timeout duration in seconds

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.

        Raises:
            BluetoothError: On failure.
        """

    @abc.abstractmethod
    def run_pairing_delegate(self, timeout_sec: Optional[int | None]) -> None:
        """Run Pairing Delegate Server calls.

        Args:
            timeout_sec: timeout duration in seconds
        """

    @abc.abstractmethod
    def set_discoverable(
        self, discoverable: bool, timeout_sec: Optional[int | None]
    ) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.
            timeout_sec: timeout duration in seconds

        Raises:
            BluetoothError: On failure.
        """
