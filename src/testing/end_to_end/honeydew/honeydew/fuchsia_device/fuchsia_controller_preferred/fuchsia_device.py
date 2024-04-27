#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using Fuchsia-Controller
preferred transport."""

import logging

from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_sl4f,
)
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_sl4f,
)
from honeydew.affordances.sl4f.wlan import wlan as wlan_sl4f
from honeydew.affordances.sl4f.wlan import wlan_policy as wlan_policy_sl4f
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_interface,
)
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_interface,
)
from honeydew.interfaces.affordances.wlan import wlan as wlan_interface
from honeydew.interfaces.affordances.wlan import (
    wlan_policy as wlan_policy_interface,
)
from honeydew.interfaces.transports import sl4f as sl4f_transport_interface
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing import custom_types
from honeydew.utils import common, properties

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDevice(fc_fuchsia_device.FuchsiaDevice):
    """FuchsiaDevice abstract base class implementation using
    Fuchsia-Controller preferred.

    That is, if a Fuchsia-Controller or FFX based implementation is available,
    use it. Otherwise use SL4F based implementation.

    Args:
        device_name: Device name returned by `ffx target list`.
        ffx_config: Config that need to be used while running FFX commands.
        device_ip_port: IP Address and port of the device.
        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.
        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Raises:
        errors.SSHCommandError: if SSH connection check fails.
        errors.FFXCommandError: if FFX connection check fails.
        errors.FuchsiaControllerError: if FC connection check fails.
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
        _LOGGER.debug(
            "Initialized Fuchsia-Controller-Preferred based FuchsiaDevice"
        )

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
    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SshConnectionError
            errors.FfxConnectionError
            errors.FuchsiaControllerConnectionError
        """
        super().health_check()
        self.sl4f.check_connection()

    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        # Restart SL4F server on the device with 60 sec retry in case of failure
        common.retry(fn=self.sl4f.start_server, timeout=60, wait_time=5)

        super().on_device_boot()
