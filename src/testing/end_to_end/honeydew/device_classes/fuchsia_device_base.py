#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Default implementation of FuchsiaDevice abstract base class."""

import base64
import logging
import os
import time
from datetime import datetime
from http.client import RemoteDisconnected
from typing import Any, Dict, Optional

from honeydew import custom_types, errors
from honeydew.affordances import bluetooth_default, component_default
from honeydew.interfaces.affordances import bluetooth as bluetooth_interface
from honeydew.interfaces.affordances import component as component_interface
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.interfaces.device_classes import (
    bluetooth_capable_device, component_capable_device, fuchsia_device)
from honeydew.transports import sl4f as sl4f_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.utils import ffx_cli, properties

_SL4F_METHODS: Dict[str, str] = {
    "GetDeviceInfo": "hwinfo_facade.HwinfoGetDeviceInfo",
    "GetProductInfo": "hwinfo_facade.HwinfoGetProductInfo",
    "GetVersion": "device_facade.GetVersion",
    "LogError": "logging_facade.LogErr",
    "LogInfo": "logging_facade.LogInfo",
    "LogWarning": "logging_facade.LogWarn",
    "Reboot": "hardware_power_statecontrol_facade.SuspendReboot",
    "Snapshot": "feedback_data_provider_facade.GetSnapshot",
}

_TIMEOUTS: Dict[str, float] = {
    "BOOT_UP_COMPLETE": 60,
    "OFFLINE": 60,
    "ONLINE": 60,
    "SNAPSHOT": 60,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDeviceBase(fuchsia_device.FuchsiaDevice,
                        component_capable_device.ComponentCapableDevice,
                        bluetooth_capable_device.BluetoothCapableDevice):
    """Default implementation of Fuchsia device abstract base class.

    This class contains methods that are supported by every device running
    Fuchsia irrespective of the device type.

    Args:
        device_name: Device name returned by `ffx target list`.
        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.
        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Attributes:
        name: Device name.
        sl4f: Object pointing to sl4f.SL4F class.
        ssh: Object pointing to ssh.SSH class

    Raises:
        errors.FuchsiaDeviceError: Failed to instantiate.
    """

    def __init__(
            self,
            device_name: str,
            ssh_private_key: str,
            ssh_user: Optional[str] = None) -> None:
        self.name: str = device_name

        self.ssh: ssh_transport.SSH = ssh_transport.SSH(
            device_name=self.name,
            username=ssh_user,
            private_key=ssh_private_key)
        self.ssh.check_connection()

        self.sl4f: sl4f_transport.SL4F = sl4f_transport.SL4F(
            device_name=self.name, ssh=self.ssh)
        self.sl4f.check_connection()

    # List all the persistent properties in alphabetical order
    @properties.PersistentProperty
    def device_type(self) -> str:
        """Returns the device type.

        Returns:
            Device type.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        return ffx_cli.get_target_type(self.name)

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
            Serial number of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_device_info_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetDeviceInfo"])
        return get_device_info_resp["result"]["serial_number"]

    # List all the dynamic properties in alphabetical order
    @properties.DynamicProperty
    def firmware_version(self) -> str:
        """Returns the firmware version of the device.

        Returns:
            Firmware version of device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_version_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetVersion"])
        return get_version_resp["result"]

    # List all the affordances in alphabetical order
    # TODO(fxbug.dev/123944): Remove this after fxbug.dev/123944 is fixed
    @properties.Affordance
    def bluetooth(self) -> bluetooth_interface.Bluetooth:
        """Returns a bluetooth affordance object.

        Returns:
            bluetooth.Bluetooth object
        """
        return bluetooth_default.BluetoothDefault(
            device_name=self.name, sl4f=self.sl4f)

    @properties.Affordance
    def component(self) -> component_interface.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """
        return component_default.ComponentDefault(
            device_name=self.name, sl4f=self.sl4f)

    # List all the public methods in alphabetical order
    def close(self) -> None:
        """Clean up method."""
        return

    def log_message_to_device(
            self, message: str, level: custom_types.LEVEL) -> None:
        """Log message to fuchsia device at specified level.

        Args:
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
        message = f"[HoneyDew] - [Host Time: {timestamp}] - {message}"
        self.sl4f.run(
            method=_SL4F_METHODS[f"Log{level.name.capitalize()}"],
            params={"message": message})

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
        _LOGGER.info("Power cycling %s...", self.name)
        self.log_message_to_device(
            message=f"Powering cycling {self.name}...",
            level=custom_types.LEVEL.INFO)

        _LOGGER.info("Powering off %s...", self.name)
        power_switch.power_off(outlet)
        self._wait_for_offline()

        _LOGGER.info("Powering on %s...", self.name)
        power_switch.power_on(outlet)
        self._wait_for_bootup_complete()

        self.log_message_to_device(
            message=f"Successfully power cycled {self.name}...",
            level=custom_types.LEVEL.INFO)

    def reboot(self) -> None:
        """Soft reboot the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        _LOGGER.info("Rebooting %s...", self.name)
        self.log_message_to_device(
            message=f"Rebooting {self.name}...", level=custom_types.LEVEL.INFO)
        self.sl4f.run(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected])
        self._wait_for_offline()
        self._wait_for_bootup_complete()
        self.log_message_to_device(
            message=f"Successfully rebooted {self.name}...",
            level=custom_types.LEVEL.INFO)

    def snapshot(
            self, directory: str, snapshot_file: Optional[str] = None) -> str:
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
            errors.FuchsiaDeviceError: On failure.
        """
        directory = os.path.abspath(directory)
        try:
            os.makedirs(directory)
        except FileExistsError:
            pass

        if not snapshot_file:
            timestamp: str = datetime.now().strftime("%Y-%m-%d-%I-%M-%S-%p")
            snapshot_file = f"Snapshot_{self.name}_{timestamp}.zip"
        snapshot_file_path: str = os.path.join(directory, snapshot_file)

        _LOGGER.info("Collecting snapshot on %s...", self.name)

        snapshot_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["Snapshot"], timeout=_TIMEOUTS["SNAPSHOT"])
        snapshot_base64_encoded_str: str = snapshot_resp["result"]["zip"]
        snapshot_base64_decoded_bytes: bytes = base64.b64decode(
            snapshot_base64_encoded_str)

        with open(snapshot_file_path, "wb") as snapshot_binary_zip:
            snapshot_binary_zip.write(snapshot_base64_decoded_bytes)

        _LOGGER.info("Snapshot file has been saved @ '%s'", snapshot_file_path)
        return snapshot_file_path

    # List all private methods in alphabetical order
    @properties.PersistentProperty
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        get_product_info_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetProductInfo"])
        return get_product_info_resp["result"]

    def _wait_for_bootup_complete(
            self, timeout: float = _TIMEOUTS["BOOT_UP_COMPLETE"]) -> None:
        """Wait for Fuchsia device to complete the boot.

        Args:
            timeout: How long in sec to wait for bootup.

        Raises:
            errors.FuchsiaDeviceError: If bootup operation(s) fail.
        """
        end_time: float = time.time() + timeout

        # wait until device is responsive to FFX
        self._wait_for_online(timeout)

        # wait until device to allow ssh connection
        remaining_timeout = max(0, end_time - time.time())
        self.ssh.check_connection(timeout=remaining_timeout)

        # Restart SL4F server on the device
        self.sl4f.start_server()

        # If applicable, initialize bluetooth stack
        if "qemu" not in self.device_type:
            self.bluetooth.sys_init()

    def _wait_for_offline(self, timeout: float = _TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not offline.
        """
        _LOGGER.info("Waiting for %s to go offline...", self.name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if not ffx_cli.check_ffx_connection(self.name):
                _LOGGER.info("%s is offline.", self.name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to go offline in {timeout}sec.")

    def _wait_for_online(self, timeout: float = _TIMEOUTS["ONLINE"]) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not online.
        """
        _LOGGER.info("Waiting for %s to go online...", self.name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if ffx_cli.check_ffx_connection(self.name):
                _LOGGER.info("%s is online.", self.name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.name}' failed to go online in {timeout}sec.")
