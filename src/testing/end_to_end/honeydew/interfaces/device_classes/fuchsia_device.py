#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Fuchsia device."""

import abc
import os
from typing import Optional

from honeydew import custom_types
from honeydew.interfaces.affordances import component
from honeydew.utils import ffx_cli

DEFAULT_SSH_USER = "fuchsia"
DEFAULT_SSH_PKEY = os.environ.get("SSH_PRIVATE_KEY_FILE")


class FuchsiaDevice(abc.ABC):
    """Abstract base class for Fuchsia device.

    This class contains abstract methods that are supported by every device
    running Fuchsia irrespective of the device type.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_pkey: Absolute path to the SSH private key file needed to SSH
            into fuchsia device. Either pass the value here or set value in
            'SSH_PRIVATE_KEY_FILE' environmental variable.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

        device_ip_address: Device IP (V4|V6) address. If not provided, attempts
            to resolve automatically.
    """

    def __init__(
            self,
            device_name: str,
            ssh_pkey: Optional[str] = DEFAULT_SSH_PKEY,
            ssh_user: str = DEFAULT_SSH_USER,
            device_ip_address: Optional[str] = None) -> None:
        if not ssh_pkey:
            raise RuntimeError(
                "ssh_pkey arg is not valid. This is needed to SSH into fuchsia "
                "device. Please either pass ssh_pkey or set this value in "
                "'SSH_PRIVATE_KEY_FILE' environmental variable")

        self.name = device_name
        self._ssh_pkey = ssh_pkey
        self._ssh_user = ssh_user
        self._ip_address = device_ip_address or ffx_cli.get_target_address(
            self.name)

    # List all the static properties in alphabetical order
    @property
    @abc.abstractmethod
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.
        """

    @property
    @abc.abstractmethod
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.
        """

    @property
    @abc.abstractmethod
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.
        """

    @property
    @abc.abstractmethod
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.
        """

    @property
    @abc.abstractmethod
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of device.
        """

    # List all the dynamic properties in alphabetical order
    @property
    @abc.abstractmethod
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.
        """

    # List all the affordances in alphabetical order
    @property
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
