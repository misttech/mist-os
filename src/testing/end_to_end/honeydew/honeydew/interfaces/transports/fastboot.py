# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via Fastboot."""

import abc

from honeydew.auxiliary_devices.power_switch import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.transports import serial as serial_interface
from honeydew.utils import properties


class Fastboot(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via Fastboot."""

    @properties.PersistentProperty
    @abc.abstractmethod
    def node_id(self) -> str:
        """Fastboot node id.

        Returns:
            Fastboot node value.
        """

    @abc.abstractmethod
    def boot_to_fastboot_mode(
        self,
        use_serial: bool = False,
        serial_transport: serial_interface.Serial | None = None,
        power_switch: power_switch_interface.PowerSwitch | None = None,
        outlet: int | None = None,
    ) -> None:
        """Boot the device to fastboot mode from fuchsia mode.

        Args:
            use_serial: Use serial port on the device to boot into Fastboot mode.
                If set to True, user need to also pass serial_transport, power_switch and outlet
                args so that device can be power cycled.
            serial_transport: Implementation of Serial interface.
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FuchsiaDeviceError: Failed to boot the device to fastboot
                mode.
        """

    @abc.abstractmethod
    def boot_to_fuchsia_mode(self) -> None:
        """Boot the device to fuchsia mode from fastboot mode.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FuchsiaDeviceError: Failed to boot the device to fuchsia
                mode.
        """

    @abc.abstractmethod
    def is_in_fastboot_mode(self) -> bool:
        """Checks if device is in fastboot mode or not.

        Returns:
            True if in fastboot mode, False otherwise.

        Raises:
            errors.FastbootCommandError: Failed to check if device is in fastboot mode or not.
        """

    @abc.abstractmethod
    def run(self, cmd: list[str]) -> list[str]:
        """Executes and returns the output of `fastboot -s {node} {cmd}`.

        Args:
            cmd: Fastboot command to run.

        Returns:
            Output of `fastboot -s {node} {cmd}`.

        Raises:
            errors.FuchsiaStateError: Invalid state to perform this operation.
            errors.FastbootCommandError: In case of failure.
        """

    @abc.abstractmethod
    def wait_for_fastboot_mode(self) -> None:
        """Wait for Fuchsia device to go to fastboot mode."""

    @abc.abstractmethod
    def wait_for_fuchsia_mode(self) -> None:
        """Wait for Fuchsia device to go to fuchsia mode."""
