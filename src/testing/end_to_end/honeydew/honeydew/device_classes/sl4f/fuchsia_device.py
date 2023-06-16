#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using SL4F."""

import base64
from datetime import datetime
from http.client import RemoteDisconnected
import logging
import os
import time
from typing import Any, Dict, Optional

from honeydew import custom_types
from honeydew import errors
from honeydew.affordances.sl4f import component as component_sl4f
from honeydew.affordances.sl4f import tracing as tracing_sl4f
from honeydew.affordances.sl4f.bluetooth import \
    bluetooth_gap as bluetooth_gap_sl4f
from honeydew.interfaces.affordances import component as component_interface
from honeydew.interfaces.affordances import tracing as tracing_interface
from honeydew.interfaces.affordances.bluetooth import \
    bluetooth_gap as bluetooth_gap_interface
from honeydew.interfaces.auxiliary_devices import \
    power_switch as power_switch_interface
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.interfaces.device_classes import transports_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import sl4f as sl4f_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.utils import properties

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


class FuchsiaDevice(fuchsia_device.FuchsiaDevice,
                    affordances_capable.BluetoothGapCapableDevice,
                    affordances_capable.ComponentCapableDevice,
                    affordances_capable.TracingCapableDevice,
                    transports_capable.FFXCapableDevice,
                    transports_capable.SL4FCapableDevice,
                    transports_capable.SSHCapableDevice):
    """FuchsiaDevice abstract base class implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.
        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Raises:
        errors.SSHCommandError: if SSH connection check fails.
        errors.FFXCommandError: if FFX connection check fails.
        errors.Sl4fError: if SL4F connection check fails.
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
    def sl4f(self) -> sl4f_transport.SL4F:
        """Returns the SL4F transport object.

        Returns:
            SL4F object.

        Raises:
            errors.Sl4fError: Failed to instantiate.
        """
        sl4f_obj: sl4f_transport.SL4F = sl4f_transport.SL4F(
            device_name=self.device_name)
        return sl4f_obj

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
    # TODO(fxbug.dev/123944): Remove this after fxbug.dev/123944 is fixed
    @properties.Affordance
    def bluetooth_gap(self) -> bluetooth_gap_interface.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """
        return bluetooth_gap_sl4f.BluetoothGap(
            device_name=self.device_name, sl4f=self.sl4f)

    @properties.Affordance
    def component(self) -> component_interface.Component:
        """Returns a component affordance object.

        Returns:
            component.Component object
        """
        return component_sl4f.Component(
            device_name=self.device_name, sl4f=self.sl4f)

    @properties.Affordance
    def tracing(self) -> tracing_interface.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
        return tracing_sl4f.Tracing(
            device_name=self.device_name, sl4f=self.sl4f)

    # List all the public methods in alphabetical order
    def close(self) -> None:
        """Clean up method."""
        return

    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SSHCommandError: if SSH connection check fails
            errors.FFXCommandError: if FFX connection check fails
            errors.Sl4fError: if SL4F connection check fails
        """
        if self._ssh_private_key:
            self.ssh.check_connection()
        self.ffx.check_connection()
        self.sl4f.check_connection()

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

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        _LOGGER.info("Power cycling %s...", self.device_name)
        self.log_message_to_device(
            message=f"Powering cycling {self.device_name}...",
            level=custom_types.LEVEL.INFO)

        _LOGGER.info("Powering off %s...", self.device_name)
        power_switch.power_off(outlet)
        self._wait_for_offline()

        _LOGGER.info("Powering on %s...", self.device_name)
        power_switch.power_on(outlet)
        self._wait_for_bootup_complete()

        self.log_message_to_device(
            message=f"Successfully power cycled {self.device_name}...",
            level=custom_types.LEVEL.INFO)

    def reboot(self) -> None:
        """Soft reboot the device.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        _LOGGER.info("Rebooting %s...", self.device_name)
        self.log_message_to_device(
            message=f"Rebooting {self.device_name}...",
            level=custom_types.LEVEL.INFO)
        self.sl4f.run(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected])
        self._wait_for_offline()
        self._wait_for_bootup_complete()
        self.log_message_to_device(
            message=f"Successfully rebooted {self.device_name}...",
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
            snapshot_file = f"Snapshot_{self.device_name}_{timestamp}.zip"
        snapshot_file_path: str = os.path.join(directory, snapshot_file)

        _LOGGER.info("Collecting snapshot on %s...", self.device_name)

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
        # wait until device is online
        self._wait_for_online(timeout)

        # Restart SL4F server on the device and check other transports are
        # available
        if self._ssh_private_key:
            self.ssh.check_connection()
        self.ffx.check_connection()
        self.sl4f.start_server()

        # If applicable, initialize bluetooth stack
        if "qemu" not in self.device_type:
            self.bluetooth_gap.sys_init()

    def _wait_for_offline(self, timeout: float = _TIMEOUTS["OFFLINE"]) -> None:
        """Wait for Fuchsia device to go offline.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not offline.
        """
        _LOGGER.info("Waiting for %s to go offline...", self.device_name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if not self.ffx.is_target_connected():
                _LOGGER.info("%s is offline.", self.device_name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.device_name}' failed to go offline in {timeout}sec.")

    def _wait_for_online(self, timeout: float = _TIMEOUTS["ONLINE"]) -> None:
        """Wait for Fuchsia device to go online.

        Args:
            timeout: How long in sec to wait for device to go offline.

        Raises:
            errors.FuchsiaDeviceError: If device is not online.
        """
        _LOGGER.info("Waiting for %s to go online...", self.device_name)
        start_time: float = time.time()
        end_time: float = start_time + timeout
        while time.time() < end_time:
            if self.ffx.is_target_connected():
                _LOGGER.info("%s is online.", self.device_name)
                break
            time.sleep(.5)
        else:
            raise errors.FuchsiaDeviceError(
                f"'{self.device_name}' failed to go online in {timeout}sec.")
