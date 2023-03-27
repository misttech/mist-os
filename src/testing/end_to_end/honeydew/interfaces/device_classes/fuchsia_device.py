#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Fuchsia device."""

import abc
from typing import Optional

from honeydew import custom_types
from honeydew.interfaces.affordances import bluetooth, component
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.utils import ffx_cli, properties

DEFAULT_SSH_USER = "fuchsia"


class FuchsiaDevice(abc.ABC):
    """Abstract base class for Fuchsia device.

    This class contains abstract methods that are supported by every device
    running Fuchsia irrespective of the device type.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

        device_ip_address: Device IP (V4|V6) address. If not provided, attempts
            to resolve automatically.

    Raises:
        errors.FuchsiaDeviceError: Failed to instantiate.
    """

    def __init__(
            self,
            device_name: str,
            ssh_private_key: str,
            ssh_user: str = DEFAULT_SSH_USER,
            device_ip_address: Optional[str] = None) -> None:
        self.name = device_name
        self._ssh_private_key = ssh_private_key
        self._ssh_user = ssh_user
        self._ip_address = device_ip_address or ffx_cli.get_target_address(
            self.name)

    # List all the persistent properties in alphabetical order
    @properties.PersistentProperty
    @abc.abstractmethod
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.
        """

    @properties.PersistentProperty
    @abc.abstractmethod
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.
        """

    # List all the dynamic properties in alphabetical order
    @properties.DynamicProperty
    @abc.abstractmethod
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.
        """

    # List all the affordances in alphabetical order
    @properties.Affordance
    @abc.abstractmethod
    def bluetooth(self) -> bluetooth.Bluetooth:
        """Returns a bluetooth affordance object.

        Returns:
            bluetooth.Bluetooth object
        """

    @properties.Affordance
    @abc.abstractmethod
    def component(self) -> component.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def close(self) -> None:
        """Clean up method."""

    @abc.abstractmethod
    def log_message_to_device(self, message: str, level: custom_types.LEVEL):
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """

    @abc.abstractmethod
    def power_cycle(
            self,
            power_switch: power_switch_interface.PowerSwitch,
            outlet: Optional[int] = None) -> None:
        """Power cycle (power off, wait for delay, power on) the device.

        Args:
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.
        """

    @abc.abstractmethod
    def reboot(self) -> None:
        """Soft reboot the device."""

    @abc.abstractmethod
    def snapshot(
            self, directory: str, snapshot_file: Optional[str] = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file need
                to be saved.

            snapshot_file: Name of the file to be used to save snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.
        """
