#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia device common implementation with transport-independent logic."""

import abc
import ipaddress
import logging
import os
from collections.abc import Callable
from datetime import datetime
from typing import Any

from honeydew import errors
from honeydew.affordances.ffx import session as session_ffx
from honeydew.affordances.ffx.ui import screenshot as screenshot_ffx
from honeydew.affordances.starnix import (
    system_power_state_controller as system_power_state_controller_starnix,
)
from honeydew.interfaces.affordances import (
    session,
    system_power_state_controller,
)
from honeydew.interfaces.affordances.ui import screenshot
from honeydew.interfaces.auxiliary_devices import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.interfaces.transports import (
    fastboot as fastboot_transport_interface,
)
from honeydew.interfaces.transports import ffx as ffx_transport_interface
from honeydew.interfaces.transports import (
    fuchsia_controller as fuchsia_controller_transport_interface,
)
from honeydew.interfaces.transports import sl4f as sl4f_transport_interface
from honeydew.interfaces.transports import ssh as ssh_transport_interface
from honeydew.transports import fastboot as fastboot_transport
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.typing import custom_types
from honeydew.utils import properties

_LOGGER: logging.Logger = logging.getLogger(__name__)


class BaseFuchsiaDevice(
    fuchsia_device_interface.FuchsiaDevice,
    affordances_capable.RebootCapableDevice,
):
    """Common implementation for Fuchsia devices using different transports.
    Every device running Fuchsia contains common functionality as well as the
    FFX and SSH transports. This logic is centralized here.
    """

    def __init__(
        self,
        device_name: str,
        ffx_config: custom_types.FFXConfig,
        device_ip_port: custom_types.IpPort | None = None,
        ssh_private_key: str | None = None,
        ssh_user: str | None = None,
    ) -> None:
        _LOGGER.debug("Initializing FuchsiaDevice")
        self._name: str = device_name

        self._ffx_config: custom_types.FFXConfig = ffx_config

        self._ip_address_port: custom_types.IpPort | None = device_ip_port

        self._ip_address: (
            ipaddress.IPv4Address | ipaddress.IPv6Address | None
        ) = None
        if self._ip_address_port:
            self._ip_address = self._ip_address_port.ip

        self._ssh_private_key: str | None = ssh_private_key
        self._ssh_user: str | None = ssh_user

        self._on_device_boot_fns: list[Callable[[], None]] = []

        self.health_check()

    # List all the persistent properties
    @properties.PersistentProperty
    def board(self) -> str:
        """Returns the board value of the device.

        Returns:
            board value of the device.

        Raises:
            errors.FfxCommandError: On failure.
        """
        return self.ffx.get_target_board()

    @properties.PersistentProperty
    def device_name(self) -> str:
        """Returns the name of the device.

        Returns:
            Name of the device.
        """
        return self._name

    @properties.PersistentProperty
    def manufacturer(self) -> str:
        """Returns the manufacturer of the device.

        Returns:
            Manufacturer of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["manufacturer"]

    @properties.PersistentProperty
    def model(self) -> str:
        """Returns the model of the device.

        Returns:
            Model of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["model"]

    @properties.PersistentProperty
    def product(self) -> str:
        """Returns the product value of the device.

        Returns:
            product value of the device.

        Raises:
            errors.FfxCommandError: On failure.
        """
        return self.ffx.get_target_product()

    @properties.PersistentProperty
    def product_name(self) -> str:
        """Returns the product name of the device.

        Returns:
            Product name of the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return self._product_info["name"]

    @properties.PersistentProperty
    def serial_number(self) -> str:
        """Returns the serial number of the device.

        Returns:
            Serial number of the device.
        """
        return self._device_info["serial_number"]

    # List all the dynamic properties
    @properties.DynamicProperty
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of the device.
        """
        return self._build_info["version"]

    # List all transports
    @properties.Transport
    def fastboot(self) -> fastboot_transport_interface.Fastboot:
        """Returns the Fastboot transport object.

        Returns:
            Fastboot transport interface implementation.

        Raises:
            errors.FuchsiaDeviceError: Failed to instantiate.
        """
        fastboot_obj: fastboot_transport_interface.Fastboot = (
            fastboot_transport.Fastboot(
                device_name=self.device_name,
                device_ip=self._ip_address,
                reboot_affordance=self,
                ffx_transport=self.ffx,
            )
        )
        return fastboot_obj

    @properties.Transport
    def ffx(self) -> ffx_transport_interface.FFX:
        """Returns the FFX transport object.

        Returns:
            FFX transport interface implementation.

        Raises:
            errors.FfxCommandError: Failed to instantiate.
        """
        ffx_obj: ffx_transport_interface.FFX = ffx_transport.FFX(
            target_name=self.device_name,
            config=self._ffx_config,
            target_ip_port=self._ip_address_port,
        )
        return ffx_obj

    @properties.Transport
    def fuchsia_controller(
        self,
    ) -> fuchsia_controller_transport_interface.FuchsiaController:
        """Returns the Fuchsia-Controller transport object.

        Returns:
            Fuchsia-Controller transport interface implementation.

        Raises:
            errors.FuchsiaControllerError: Failed to instantiate.
        """
        raise NotImplementedError

    @properties.Transport
    def ssh(self) -> ssh_transport_interface.SSH:
        """Returns the SSH transport object.

        Returns:
            SSH transport interface implementation.

        Raises:
            errors.SSHCommandError: Failed to instantiate.
        """
        if not self._ssh_private_key:
            raise errors.SSHCommandError(
                "ssh_private_key argument need to be passed during device "
                "init in-order to SSH into the device"
            )
        ssh_obj: ssh_transport_interface.SSH = ssh_transport.SSH(
            device_name=self.device_name,
            ip_port=self._ip_address_port,
            username=self._ssh_user,
            private_key=self._ssh_private_key,
            ffx_transport=self.ffx,
        )
        return ssh_obj

    @properties.Transport
    def sl4f(self) -> sl4f_transport_interface.SL4F:
        """Returns the SL4F transport object.

        Returns:
            SL4F transport interface implementation.

        Raises:
            errors.Sl4fError: Failed to instantiate.
        """
        raise NotImplementedError

    # List all the affordances
    @properties.Affordance
    def session(self) -> session.Session:
        """Returns a session affordance object.

        Returns:
            session.Session object
        """
        return session_ffx.Session(device_name=self.device_name, ffx=self.ffx)

    @properties.Affordance
    def screenshot(self) -> screenshot.Screenshot:
        """Returns a screenshot affordance object.

        Returns:
            screenshot.Screenshot object
        """
        return screenshot_ffx.Screenshot(self.ffx)

    @properties.Affordance
    def system_power_state_controller(
        self,
    ) -> system_power_state_controller.SystemPowerStateController:
        """Returns a SystemPowerStateController affordance object.

        Returns:
            system_power_state_controller.SystemPowerStateController object

        Raises:
            errors.NotSupportedError: If Fuchsia device does not support Starnix
        """
        return system_power_state_controller_starnix.SystemPowerStateController(
            device_name=self.device_name, ffx=self.ffx
        )

    # List all the public methods
    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SshConnectionError
            errors.FfxConnectionError
        """
        if self._ssh_private_key:
            self.ssh.check_connection()
        self.ffx.check_connection()

    def log_message_to_device(
        self, message: str, level: custom_types.LEVEL
    ) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """
        timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
        message = f"[Host Time: {timestamp}] - {message}"
        self._send_log_command(tag="lacewing", message=message, level=level)

    @abc.abstractmethod
    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """
        for on_device_boot_fn in self._on_device_boot_fns:
            _LOGGER.info("Calling %s", on_device_boot_fn.__qualname__)
            on_device_boot_fn()

    def power_cycle(
        self,
        power_switch: power_switch_interface.PowerSwitch,
        outlet: int | None = None,
    ) -> None:
        """Power cycle (power off, wait for delay, power on) the device.

        Args:
            power_switch: Implementation of PowerSwitch interface.
            outlet (int): If required by power switch hardware, outlet on
                power switch hardware where this fuchsia device is connected.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """
        _LOGGER.info("Power cycling %s...", self.device_name)

        try:
            self.log_message_to_device(
                message=f"Powering cycling {self.device_name}...",
                level=custom_types.LEVEL.INFO,
            )
        except Exception:  # pylint: disable=broad-except
            # power_cycle can be used as a recovery mechanism when device is
            # unhealthy. So any calls to device prior to power_cycle can
            # fail in such cases and thus ignore them.
            pass

        _LOGGER.info("Powering off %s...", self.device_name)
        power_switch.power_off(outlet)
        self.wait_for_offline()

        _LOGGER.info("Powering on %s...", self.device_name)
        power_switch.power_on(outlet)
        self.wait_for_online()

        self.on_device_boot()

        self.log_message_to_device(
            message=f"Successfully power cycled {self.device_name}...",
            level=custom_types.LEVEL.INFO,
        )

    def reboot(self) -> None:
        """Soft reboot the device.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """
        _LOGGER.info("Rebooting %s...", self.device_name)
        self.log_message_to_device(
            message=f"Rebooting {self.device_name}...",
            level=custom_types.LEVEL.INFO,
        )

        self._send_reboot_command()

        self.wait_for_offline()
        self.wait_for_online()
        self.on_device_boot()

        self.log_message_to_device(
            message=f"Successfully rebooted {self.device_name}...",
            level=custom_types.LEVEL.INFO,
        )

    def register_for_on_device_boot(self, fn: Callable[[], None]) -> None:
        """Register a function that will be called in on_device_boot."""
        self._on_device_boot_fns.append(fn)

    def snapshot(self, directory: str, snapshot_file: str | None = None) -> str:
        """Captures the snapshot of the device.

        Args:
            directory: Absolute path on the host where snapshot file will be
                saved. If this directory does not exist, this method will create
                it.

            snapshot_file: Name of the output snapshot file.
                If not provided, API will create a name using
                "Snapshot_{device_name}_{'%Y-%m-%d-%I-%M-%S-%p'}" format.

        Returns:
            Absolute path of the snapshot file.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """
        _LOGGER.info("Collecting snapshot on %s...", self.device_name)
        # Take the snapshot before creating the directory or file, as
        # _send_snapshot_command may raise an exception.
        snapshot_bytes: bytes = self._send_snapshot_command()

        directory = os.path.abspath(directory)
        try:
            os.makedirs(directory)
        except FileExistsError:
            pass

        if not snapshot_file:
            timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
            snapshot_file = f"Snapshot_{self.device_name}_{timestamp}.zip"
        snapshot_file_path: str = os.path.join(directory, snapshot_file)

        with open(snapshot_file_path, "wb") as snapshot_binary_zip:
            snapshot_binary_zip.write(snapshot_bytes)

        _LOGGER.info("Snapshot file has been saved @ '%s'", snapshot_file_path)
        return snapshot_file_path

    def wait_for_offline(
        self, timeout: float = fuchsia_device_interface.TIMEOUTS["OFFLINE"]
    ) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not offline.
        """
        _LOGGER.info("Waiting for %s to go offline...", self.device_name)
        try:
            self.ffx.wait_for_rcs_disconnection(timeout=timeout)
            _LOGGER.info("%s is offline.", self.device_name)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaDeviceError(
                f"'{self.device_name}' failed to go offline in {timeout}sec."
            ) from err

    def wait_for_online(
        self, timeout: float = fuchsia_device_interface.TIMEOUTS["ONLINE"]
    ) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not online.
        """
        _LOGGER.info("Waiting for %s to go online...", self.device_name)
        try:
            self.ffx.wait_for_rcs_connection(timeout=timeout)
            _LOGGER.info("%s is online.", self.device_name)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaDeviceError(
                f"'{self.device_name}' failed to go online in {timeout}sec."
            ) from err

    # List all private properties
    @property
    @abc.abstractmethod
    def _build_info(self) -> dict[str, Any]:
        """Returns the build information of the device.

        Returns:
            Build info dict.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """

    @property
    @abc.abstractmethod
    def _device_info(self) -> dict[str, Any]:
        """Returns the device information of the device.

        Returns:
            Device info dict.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """

    @property
    @abc.abstractmethod
    def _product_info(self) -> dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """

    # List all private methods,
    @abc.abstractmethod
    def _send_log_command(
        self, tag: str, message: str, level: custom_types.LEVEL
    ) -> None:
        """Send a device command to write to the syslog.

        Args:
            tag: Tag to apply to the message in the syslog.
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """

    @abc.abstractmethod
    def _send_reboot_command(self) -> None:
        """Send a device command to trigger a soft reboot.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.
        """

    @abc.abstractmethod
    def _send_snapshot_command(self) -> bytes:
        """Send a device command to take a snapshot.

        Raises:
            errors.FuchsiaControllerError: On communications failure.
            errors.Sl4FError: On communications failure.

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
