#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using SL4F."""

import base64
from http.client import RemoteDisconnected
import logging
from typing import Any, Dict, Optional

from honeydew import custom_types
from honeydew.affordances.sl4f import tracing as tracing_sl4f
from honeydew.affordances.sl4f.bluetooth import \
    bluetooth_gap as bluetooth_gap_sl4f
from honeydew.affordances.sl4f.ui import screenshot as screenshot_sl4f
from honeydew.affordances.sl4f.ui import user_input as user_input_sl4f
from honeydew.device_classes import base_fuchsia_device
from honeydew.interfaces.affordances import tracing as tracing_interface
from honeydew.interfaces.affordances.bluetooth import \
    bluetooth_gap as bluetooth_gap_interface
from honeydew.interfaces.affordances.ui import \
    screenshot as screenshot_interface
from honeydew.interfaces.affordances.ui import \
    user_input as user_input_interface
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import transports_capable
from honeydew.transports import sl4f as sl4f_transport
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
    "SNAPSHOT": 60,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDevice(base_fuchsia_device.BaseFuchsiaDevice,
                    affordances_capable.BluetoothGapCapableDevice,
                    affordances_capable.ScreenshotCapableDevice,
                    affordances_capable.TracingCapableDevice,
                    affordances_capable.UserInputCapableDevice,
                    transports_capable.SL4FCapableDevice):
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
        super().__init__(device_name, ssh_private_key, ssh_user)
        _LOGGER.debug("Initializing SL4F-based FuchsiaDevice")

    # List all the transports in alphabetical order
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

    # List all the affordances in alphabetical order
    @properties.Affordance
    def bluetooth_gap(self) -> bluetooth_gap_interface.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """
        return bluetooth_gap_sl4f.BluetoothGap(
            device_name=self.device_name,
            sl4f=self.sl4f,
            reboot_affordance=self)

    @properties.Affordance
    def screenshot(self) -> screenshot_interface.Screenshot:
        """Returns a screenshot affordance object.

        Returns:
            screenshot.Screenshot object
        """
        return screenshot_sl4f.Screenshot(sl4f=self.sl4f)

    @properties.Affordance
    def tracing(self) -> tracing_interface.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
        return tracing_sl4f.Tracing(
            device_name=self.device_name,
            sl4f=self.sl4f,
            reboot_affordance=self)

    @properties.Affordance
    def user_input(self) -> user_input_interface.UserInput:
        """Returns an user input affordance object.

        Returns:
            user_input.UserInput object
        """
        return user_input_sl4f.UserInput()

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
        super().health_check()
        self.sl4f.check_connection()

    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        # Restart SL4F server on the device
        self.sl4f.start_server()

        # Ensure device is healthy
        self.health_check()

        super().on_device_boot()

    # List all private properties in alphabetical order
    @property
    def _build_info(self) -> Dict[str, Any]:
        """Returns the build information of the device.

        Returns:
            Build info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_version_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetVersion"])
        return {"version": get_version_resp["result"]}

    @property
    def _device_info(self) -> Dict[str, Any]:
        """Returns the device information of the device.

        Returns:
            Device info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_device_info_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetDeviceInfo"])
        return get_device_info_resp["result"]

    @property
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_product_info_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetProductInfo"])
        return get_product_info_resp["result"]

    # List all private methods in alphabetical order
    def _send_log_command(
            self, tag: str, message: str, level: custom_types.LEVEL) -> None:
        """Send a device command to write to the syslog.

        Args:
            tag: Tag to apply to the message in the syslog.
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.Sl4fError: if SL4F command fails
        """
        message = f"[{tag}] - {message}"
        self.sl4f.run(
            method=_SL4F_METHODS[f"Log{level.name.capitalize()}"],
            params={"message": message})

    def _send_reboot_command(self) -> None:
        """Send a device command to trigger a soft reboot.

        Raises:
            errors.Sl4fError: if SL4F command fails
        """
        self.sl4f.run(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected])

    def _send_snapshot_command(self) -> bytes:
        """Send a device command to take a snapshot.

        Raises:
            errors.Sl4fError: if SL4F command fails

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
        snapshot_resp: Dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["Snapshot"], timeout=_TIMEOUTS["SNAPSHOT"])
        snapshot_base64_encoded_str: str = snapshot_resp["result"]["zip"]
        return base64.b64decode(snapshot_base64_encoded_str)
