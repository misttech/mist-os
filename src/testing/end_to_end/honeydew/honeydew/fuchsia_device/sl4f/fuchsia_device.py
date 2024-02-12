#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using SL4F."""

import base64
import logging
from http.client import RemoteDisconnected
from typing import Any

from honeydew.affordances.sl4f import tracing as tracing_sl4f
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_sl4f,
)
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_sl4f,
)
from honeydew.affordances.sl4f.ui import user_input as user_input_sl4f
from honeydew.affordances.sl4f.wlan import wlan as wlan_sl4f
from honeydew.affordances.sl4f.wlan import wlan_policy as wlan_policy_sl4f
from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.interfaces.affordances import rtc as rtc_interface
from honeydew.interfaces.affordances import tracing as tracing_interface
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_interface,
)
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_interface,
)
from honeydew.interfaces.affordances.ui import (
    user_input as user_input_interface,
)
from honeydew.interfaces.affordances.wlan import wlan as wlan_interface
from honeydew.interfaces.affordances.wlan import (
    wlan_policy as wlan_policy_interface,
)
from honeydew.interfaces.transports import sl4f as sl4f_transport_interface
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing import custom_types
from honeydew.utils import common, properties

_SL4F_METHODS: dict[str, str] = {
    "GetDeviceInfo": "hwinfo_facade.HwinfoGetDeviceInfo",
    "GetProductInfo": "hwinfo_facade.HwinfoGetProductInfo",
    "GetVersion": "device_facade.GetVersion",
    "LogError": "logging_facade.LogErr",
    "LogInfo": "logging_facade.LogInfo",
    "LogWarning": "logging_facade.LogWarn",
    "Reboot": "hardware_power_statecontrol_facade.SuspendReboot",
    "Snapshot": "feedback_data_provider_facade.GetSnapshot",
}

_TIMEOUTS: dict[str, float] = {
    "SNAPSHOT": 60,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDevice(base_fuchsia_device.BaseFuchsiaDevice):
    """FuchsiaDevice abstract base class implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        device_ip: IP Address of the device.
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
        ffx_config: custom_types.FFXConfig,
        device_ip_port: custom_types.IpPort | None = None,
        ssh_private_key: str | None = None,
        ssh_user: str | None = None,
    ) -> None:
        super().__init__(
            device_name, ffx_config, device_ip_port, ssh_private_key, ssh_user
        )
        _LOGGER.debug("Initialized SL4F-based FuchsiaDevice")

    # List all the transports
    @properties.Transport
    def sl4f(self) -> sl4f_transport_interface.SL4F:
        """Returns the SL4F transport object.

        Returns:
            SL4F transport interface implementation.

        Raises:
            errors.Sl4fError: Failed to instantiate.
        """
        sl4f_obj: sl4f_transport_interface.SL4F = sl4f_transport.SL4F(
            device_name=self.device_name,
            device_ip=self._ip_address,
            ffx_transport=self.ffx,
        )
        return sl4f_obj

    # List all the affordances
    @properties.Affordance
    def bluetooth_avrcp(self) -> bluetooth_avrcp_interface.BluetoothAvrcp:
        """Returns a BluetoothAvrcp affordance object.

        Returns:
            bluetooth_avrcp.BluetoothAvrcp object
        """
        return bluetooth_avrcp_sl4f.BluetoothAvrcp(
            device_name=self.device_name, sl4f=self.sl4f, reboot_affordance=self
        )

    @properties.Affordance
    def bluetooth_gap(self) -> bluetooth_gap_interface.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """
        return bluetooth_gap_sl4f.BluetoothGap(
            device_name=self.device_name, sl4f=self.sl4f, reboot_affordance=self
        )

    @properties.Affordance
    def rtc(self) -> rtc_interface.Rtc:
        """Returns an Rtc affordance object.

        Raises:
            NotImplementedError: Unsupported on SL4F.
        """
        raise NotImplementedError("SL4F unsupported, use FuchsiaController")

    @properties.Affordance
    def tracing(self) -> tracing_interface.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
        return tracing_sl4f.Tracing(
            device_name=self.device_name, sl4f=self.sl4f, reboot_affordance=self
        )

    @properties.Affordance
    def user_input(self) -> user_input_interface.UserInput:
        """Returns an user input affordance object.

        Returns:
            user_input.UserInput object
        """
        return user_input_sl4f.UserInput(sl4f=self.sl4f)

    @properties.Affordance
    def wlan_policy(self) -> wlan_policy_interface.WlanPolicy:
        """Returns a wlan_policy affordance object.

        Returns:
            wlan_policy.WlanPolicy object
        """
        return wlan_policy_sl4f.WlanPolicy(
            device_name=self.device_name, sl4f=self.sl4f
        )

    @properties.Affordance
    def wlan(self) -> wlan_interface.Wlan:
        """Returns a wlan affordance object.

        Returns:
            wlan.Wlan object
        """
        return wlan_sl4f.Wlan(device_name=self.device_name, sl4f=self.sl4f)

    # List all the public methods
    def close(self) -> None:
        """Clean up method."""
        return

    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SshConnectionError
            errors.FfxConnectionError
            errors.Sl4fConnectionError
        """
        super().health_check()
        self.sl4f.check_connection()

    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        # Restart SL4F server on the device with 60 sec retry in case of failure
        common.retry(fn=self.sl4f.start_server, timeout=60, wait_time=5)

        # Ensure device is healthy
        self.health_check()

        super().on_device_boot()

    # List all private properties
    @property
    def _build_info(self) -> dict[str, Any]:
        """Returns the build information of the device.

        Returns:
            Build info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_version_resp: dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetVersion"]
        )
        return {"version": get_version_resp["result"]}

    @property
    def _device_info(self) -> dict[str, Any]:
        """Returns the device information of the device.

        Returns:
            Device info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_device_info_resp: dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetDeviceInfo"]
        )
        return get_device_info_resp["result"]

    @property
    def _product_info(self) -> dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.Sl4fError: On SL4F communication failure.
        """
        get_product_info_resp: dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["GetProductInfo"]
        )
        return get_product_info_resp["result"]

    # List all private methods
    def _send_log_command(
        self, tag: str, message: str, level: custom_types.LEVEL
    ) -> None:
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
            params={"message": message},
        )

    def _send_reboot_command(self) -> None:
        """Send a device command to trigger a soft reboot.

        Raises:
            errors.Sl4fError: if SL4F command fails
        """
        self.sl4f.run(
            method=_SL4F_METHODS["Reboot"],
            exceptions_to_skip=[RemoteDisconnected],
        )

    def _send_snapshot_command(self) -> bytes:
        """Send a device command to take a snapshot.

        Raises:
            errors.Sl4fError: if SL4F command fails

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
        # Ensure device is healthy and ready to accept SL4F requests before
        # sending snapshot command.
        self.sl4f.start_server()
        self.health_check()

        snapshot_resp: dict[str, Any] = self.sl4f.run(
            method=_SL4F_METHODS["Snapshot"], timeout=_TIMEOUTS["SNAPSHOT"]
        )
        snapshot_base64_encoded_str: str = snapshot_resp["result"]["zip"]
        return base64.b64decode(snapshot_base64_encoded_str)
