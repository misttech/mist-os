#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using Fuchsia-Controller."""

from typing import Optional

from honeydew import custom_types
from honeydew import errors
from honeydew.affordances.fuchsia_controller import bluetooth as bluetooth_fc
from honeydew.affordances.fuchsia_controller import component as component_fc
from honeydew.affordances.fuchsia_controller import tracing as tracing_fc
from honeydew.interfaces.affordances import bluetooth
from honeydew.interfaces.affordances import component
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.interfaces.device_classes import transports_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.utils import properties


class FuchsiaDevice(fuchsia_device.FuchsiaDevice,
                    affordances_capable.BluetoothCapableDevice,
                    affordances_capable.ComponentCapableDevice,
                    affordances_capable.TracingCapableDevice,
                    transports_capable.FFXCapableDevice,
                    transports_capable.SSHCapableDevice):
    """FuchsiaDevice abstract base class implementation using
    Fuchsia-Controller.

    Args:
        device_name: Device name returned by `ffx target list`.
        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.
        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Raises:
        errors.SSHCommandError: if SSH connection check fails.
        errors.FFXCommandError: if FFX connection check fails.
    """

    def __init__(
            self,
            device_name: str,
            ssh_private_key: Optional[str] = None,
            ssh_user: Optional[str] = None) -> None:
        self._name: str = device_name

        self._ssh_private_key: Optional[str] = ssh_private_key
        self._ssh_user: Optional[str] = ssh_user

        self.health_check()

    # List all the persistent properties in alphabetical order
    @properties.PersistentProperty
    def device_name(self) -> str:
        """Returns the device name.

        Returns:
            Device name.
        """
        return self._name

    @properties.PersistentProperty
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FfxCommandError: In case of failure.
        """
        return self.ffx.get_target_type()

    @properties.PersistentProperty
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of the device.
        """
        raise NotImplementedError

    @properties.PersistentProperty
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of the device.
        """
        raise NotImplementedError

    @properties.PersistentProperty
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.
        """
        raise NotImplementedError

    @properties.PersistentProperty
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of the device.
        """
        raise NotImplementedError

    # List all the dynamic properties in alphabetical order
    @properties.DynamicProperty
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of the device.
        """
        raise NotImplementedError

    # List all the transports in alphabetical order
    @properties.Transport
    def ffx(self) -> ffx_transport.FFX:
        """Returns the FFX transport object.

        Returns:
            FFX object.

        Raises:
            errors.Sl4fError: Failed to instantiate.
        """
        ffx_obj: ffx_transport.FFX = ffx_transport.FFX(target=self.device_name)
        return ffx_obj

    @properties.Transport
    def ssh(self) -> ssh_transport.SSH:
        """Returns the SSH transport object.

        Returns:
            SSH object.
        """
        if not self._ssh_private_key:
            raise errors.SSHCommandError(
                "ssh_private_key argument need to be passed during device " \
                "init in-order to SSH into the device"
            )

        ssh_obj: ssh_transport.SSH = ssh_transport.SSH(
            device_name=self.device_name,
            username=self._ssh_user,
            private_key=self._ssh_private_key)
        return ssh_obj

    # List all the affordances in alphabetical order
    @properties.Affordance
    def bluetooth(self) -> bluetooth.Bluetooth:
        """Returns a bluetooth affordance object.

        Returns:
            bluetooth.Bluetooth object
        """
        return bluetooth_fc.Bluetooth()

    @properties.Affordance
    def component(self) -> component.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """
        return component_fc.Component()

    @properties.Affordance
    def tracing(self) -> tracing.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
        return tracing_fc.Tracing()

    # List all the public methods in alphabetical order
    def close(self) -> None:
        """Clean up method."""
        return

    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SSHCommandError: if SSH connection check fails
            errors.FFXCommandError: if FFX connection check fails
        """
        if self._ssh_private_key:
            self.ssh.check_connection()
        self.ffx.check_connection()

    def log_message_to_device(
            self, message: str, level: custom_types.LEVEL) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.
        """
        raise NotImplementedError

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
        raise NotImplementedError

    def reboot(self) -> None:
        """Soft reboot the device."""
        raise NotImplementedError

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
        raise NotImplementedError
