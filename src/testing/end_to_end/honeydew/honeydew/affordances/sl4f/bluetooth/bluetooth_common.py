# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth Common affordance implementation using SL4F."""

from enum import StrEnum
from typing import Any

from honeydew import errors
from honeydew.interfaces.affordances.bluetooth import bluetooth_common
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing import bluetooth


class Sl4fMethods(StrEnum):
    SET_DISCOVERABLE = "bt_sys_facade.BluetoothSetDiscoverable"
    INIT_SYS = "bt_sys_facade.BluetoothInitSys"
    REQUEST_DISCOVERY = "bt_sys_facade.BluetoothRequestDiscovery"
    GET_ACTIVE_ADDRESS = "bt_sys_facade.BluetoothGetActiveAdapterAddress"
    GET_KNOWN_REMOTE_DEVICES = "bt_sys_facade.BluetoothGetKnownRemoteDevices"
    ACCEPT_PAIRING = "bt_sys_facade.BluetoothAcceptPairing"
    PAIR_DEVICE = "bt_sys_facade.BluetoothPairDevice"
    CONNECT_DEVICE = "bt_sys_facade.BluetoothConnectDevice"
    FORGET_DEVICE = "bt_sys_facade.BluetoothForgetDevice"


class BluetoothCommon(bluetooth_common.BluetoothCommon):
    """Bluetooth Common affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
        reboot_affordance: Object that implements RebootCapableDevice.
    """

    def __init__(
        self,
        device_name: str,
        sl4f: sl4f_transport.SL4F,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        self._name: str = device_name
        self._sl4f: sl4f_transport.SL4F = sl4f
        self._reboot_affordance: affordances_capable.RebootCapableDevice = (
            reboot_affordance
        )

        # `sys_init` need to be called on every device bootup
        self._reboot_affordance.register_for_on_device_boot(fn=self.sys_init)

        # Initialize the bluetooth stack
        self.sys_init()

    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Note: This method is called automatically:
            1. During this class initialization
            2. After the device reboot

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(method=Sl4fMethods.INIT_SYS)
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete sys_init SL4F call on {self._name}."
            ) from e

    def accept_pairing(
        self,
        input_mode: bluetooth.BluetoothAcceptPairing,
        output_mode: bluetooth.BluetoothAcceptPairing,
    ) -> None:
        """Sets device to accept Bluetooth pairing.

        Args:
            input_mode: input mode of device
            output_mode: output mode of device

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.ACCEPT_PAIRING,
                params={"input": input_mode, "output": output_mode},
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete accept_pairing SL4F call on {self._name}."
            ) from e

    def connect_device(
        self,
        identifier: str,
        connection_type: bluetooth.BluetoothConnectionType,
    ) -> None:
        """Connect device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            connection_type: type of bluetooth connection

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.CONNECT_DEVICE,
                params={
                    "identifier": identifier,
                    "transport": connection_type.value,
                },
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete connect_device SL4F call on {self._name}."
            ) from e

    def forget_device(self, identifier: str) -> None:
        """Forget device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.FORGET_DEVICE,
                params={"identifier": identifier},
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete forget_device SL4F call on {self._name}."
            ) from e

    def get_active_adapter_address(self) -> str:
        """Retrieves the active adapter mac address

        Sample result:
            {"result": "[address (public) 20:1F:3B:62:E9:D2]"}
        Returns:
            The mac address of the active adapter

        Raises:
            BluetoothError: On failure.
            KeyError: On unexpected SL4F response
            AttributeError: On unexpected SL4F response
            IndexError: On unexpected SL4F response
        """
        try:
            address = self._sl4f.run(method=Sl4fMethods.GET_ACTIVE_ADDRESS)
            mac_address = address["result"].strip("[]").split(" ")
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                "Failed to complete get_active_adapter_address SL4F call on "
                f"{self._name}."
            ) from e
        return mac_address[2]

    def get_connected_devices(self) -> list[str]:
        """Retrieves all connected remote devices.

        Returns:
            A list of all connected devices by identifier. If none,
            then returns empty list.

        Raises:
            BluetoothError: On failure.
        """
        try:
            data = self._sl4f.run(method=Sl4fMethods.GET_KNOWN_REMOTE_DEVICES)
            connected_devices = []
            for value in data.get("result", {}).values():
                if value["bonded"]:
                    connected_devices.append(value["id"])
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete get_connected_devices SL4F call on {self._name}."
            ) from e
        return connected_devices

    def get_known_remote_devices(self) -> dict[str, Any]:
        """Retrieves all known remote devices received by device.

        Returns:
            A dict of all known remote devices.

        Raises:
            BluetoothError: On failure.
            KeyError: If the Sl4f call returns no "result".
        """
        try:
            known_devices = self._sl4f.run(
                method=Sl4fMethods.GET_KNOWN_REMOTE_DEVICES
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                "Failed to complete get_known_remote_devices SL4F call on "
                f"{self._name}."
            ) from e
        return known_devices["result"]

    def pair_device(
        self,
        identifier: str,
        connection_type: bluetooth.BluetoothConnectionType,
    ) -> None:
        """Pair device to target remote device via Bluetooth.

        Args:
            identifier: the identifier of target remote device.
            connection_type: type of bluetooth connection

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.PAIR_DEVICE,
                params={
                    "identifier": identifier,
                    "transport": connection_type.value,
                },
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete pair_device SL4F call on {self._name}."
            ) from e

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.REQUEST_DISCOVERY,
                params={"discovery": discovery},
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete request_discovery SL4F call on {self._name}."
            ) from e

    def set_discoverable(self, discoverable: bool) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.

        Raises:
            BluetoothError: On failure.
        """
        try:
            self._sl4f.run(
                method=Sl4fMethods.SET_DISCOVERABLE,
                params={"discoverable": discoverable},
            )
        except errors.Sl4fError as e:
            raise errors.BluetoothError(
                f"Failed to complete set_discoverable SL4F call on {self._name}."
            ) from e

    def run_pairing_delegate(self) -> None:
        """Function to run pairing delegate server calls.

        Fuchsia Controller only implementation
        """
