#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth affordance implementation using Fuchsia-Controller."""

from honeydew.custom_types import BluetoothAcceptPairing
from honeydew.custom_types import BluetoothTransport
from honeydew.interfaces.affordances.bluetooth import bluetooth_common


class BluetoothCommon(bluetooth_common.BluetoothCommon):
    """BluetoothGap affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def accept_pairing(
            self, input_mode: BluetoothAcceptPairing,
            output_mode: BluetoothAcceptPairing) -> None:
        """Sets device to accept Bluetooth pairing.

        Args:
            input_mode: input mode of device
            output_mode: output mode of device

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def connect_device(
            self, identifier: str, transport: BluetoothTransport) -> None:
        """Connect device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            transport:
                1 -> Bluetooth classic transport.
                2 -> Bluetooth LE (low energy) transport.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def forget_device(self, identifier: str) -> None:
        """Forget device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            transport:
                1 -> Bluetooth classic transport.
                2 -> Bluetooth LE (low energy) transport.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def get_active_adapter_address(self) -> str:
        """ Retrieves the active adapter mac address

        Returns:
            The mac address of the active adapter

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def get_known_remote_devices(self) -> dict:
        """Retrieves all known remote devices received by device.

        Returns:
            A dict of all known remote devices.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def pair_device(
            self, identifier: str, transport: BluetoothTransport) -> None:
        """Pair device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            transport:
                1 -> Bluetooth classic transport.
                2 -> Bluetooth LE (low energy) transport.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def set_discoverable(self, discoverable: bool) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError
