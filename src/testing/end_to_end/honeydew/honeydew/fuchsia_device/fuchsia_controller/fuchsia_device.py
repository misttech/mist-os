#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using Fuchsia-Controller."""

import asyncio
import logging
from typing import Any

import fidl.fuchsia_buildinfo as f_buildinfo
import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fidl.fuchsia_diagnostics as f_diagnostics
import fidl.fuchsia_feedback as f_feedback
import fidl.fuchsia_hardware_power_statecontrol as fhp_statecontrol
import fidl.fuchsia_hwinfo as f_hwinfo
import fidl.fuchsia_io as f_io
import fuchsia_controller_py as fcp

from honeydew import errors
from honeydew.affordances.fuchsia_controller import rtc as rtc_fc
from honeydew.affordances.fuchsia_controller import tracing as tracing_fc
from honeydew.affordances.fuchsia_controller.ui import (
    user_input as user_input_fc,
)
from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.interfaces.affordances import rtc, tracing
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_interface,
)
from honeydew.interfaces.affordances.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_interface,
)
from honeydew.interfaces.affordances.ui import user_input
from honeydew.interfaces.affordances.wlan import wlan, wlan_policy
from honeydew.interfaces.transports import (
    fuchsia_controller as fuchsia_controller_transport_interface,
)
from honeydew.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)
from honeydew.typing import custom_types
from honeydew.utils import properties

_FC_PROXIES: dict[str, custom_types.FidlEndpoint] = {
    "BuildInfo": custom_types.FidlEndpoint(
        "/core/build-info", "fuchsia.buildinfo.Provider"
    ),
    "DeviceInfo": custom_types.FidlEndpoint(
        "/core/hwinfo", "fuchsia.hwinfo.Device"
    ),
    "Feedback": custom_types.FidlEndpoint(
        "/core/feedback", "fuchsia.feedback.DataProvider"
    ),
    "ProductInfo": custom_types.FidlEndpoint(
        "/core/hwinfo", "fuchsia.hwinfo.Product"
    ),
    "PowerAdmin": custom_types.FidlEndpoint(
        "/bootstrap/shutdown_shim", "fuchsia.hardware.power.statecontrol.Admin"
    ),
    "RemoteControl": custom_types.FidlEndpoint(
        "/core/remote-control", "fuchsia.developer.remotecontrol.RemoteControl"
    ),
}

_LOG_SEVERITIES: dict[custom_types.LEVEL, f_diagnostics.Severity] = {
    custom_types.LEVEL.INFO: f_diagnostics.Severity.INFO,
    custom_types.LEVEL.WARNING: f_diagnostics.Severity.WARN,
    custom_types.LEVEL.ERROR: f_diagnostics.Severity.ERROR,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaDevice(base_fuchsia_device.BaseFuchsiaDevice):
    """FuchsiaDevice abstract base class implementation using
    Fuchsia-Controller.

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
        _LOGGER.debug("Initialized Fuchsia-Controller based FuchsiaDevice")

    # List all the transports
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
        fuchsia_controller_obj: (
            fuchsia_controller_transport_interface.FuchsiaController
        ) = fuchsia_controller_transport.FuchsiaController(
            device_name=self.device_name,
            device_ip=self._ip_address,
            ffx_transport=self.ffx,
        )
        return fuchsia_controller_obj

    # List all the affordances
    @properties.Affordance
    def bluetooth_avrcp(self) -> bluetooth_avrcp_interface.BluetoothAvrcp:
        """Returns a BluetoothAvrcp affordance object.

        Returns:
            bluetooth_avrcp.BluetoothAvrcp object
        """
        raise NotImplementedError

    @properties.Affordance
    def bluetooth_gap(self) -> bluetooth_gap_interface.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """
        raise NotImplementedError

    @properties.Affordance
    def rtc(self) -> rtc.Rtc:
        """Returns an Rtc affordance object.

        Returns:
            rtc.Rtc object
        """
        return rtc_fc.Rtc(
            fuchsia_controller=self.fuchsia_controller,
            reboot_affordance=self,
        )

    @properties.Affordance
    def tracing(self) -> tracing.Tracing:
        """Returns a tracing affordance object.

        Returns:
            tracing.Tracing object
        """
        return tracing_fc.Tracing(
            device_name=self.device_name,
            fuchsia_controller=self.fuchsia_controller,
            reboot_affordance=self,
        )

    @properties.Affordance
    def user_input(self) -> user_input.UserInput:
        """Returns an user input affordance object.

        Returns:
            user_input.UserInput object
        """
        return user_input_fc.UserInput(
            device_name=self.device_name,
            fuchsia_controller=self.fuchsia_controller,
            ffx_transport=self.ffx,
        )

    @properties.Affordance
    def wlan_policy(self) -> wlan_policy.WlanPolicy:
        """Returns a wlan_policy affordance object.

        Returns:
            wlan_policy.WlanPolicy object
        """
        raise NotImplementedError

    @properties.Affordance
    def wlan(self) -> wlan.Wlan:
        """Returns a wlan affordance object.

        Returns:
            wlan.Wlan object
        """
        raise NotImplementedError

    # List all the public methods
    def close(self) -> None:
        """Clean up method."""
        return

    def health_check(self) -> None:
        """Ensure device is healthy.

        Raises:
            errors.SshConnectionError
            errors.FfxConnectionError
            errors.FuchsiaControllerConnectionError
        """
        super().health_check()
        self.fuchsia_controller.check_connection()

    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        # Create a new Fuchsia controller context for new device connection.
        self.fuchsia_controller.create_context()

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
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            buildinfo_provider_proxy = f_buildinfo.Provider.Client(
                self.fuchsia_controller.connect_device_proxy(
                    _FC_PROXIES["BuildInfo"]
                )
            )
            build_info_resp = asyncio.run(
                buildinfo_provider_proxy.get_build_info()
            )
            return build_info_resp.build_info
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status

    @property
    def _device_info(self) -> dict[str, Any]:
        """Returns the device information of the device.

        Returns:
            Device info dict.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            hwinfo_device_proxy = f_hwinfo.Device.Client(
                self.fuchsia_controller.connect_device_proxy(
                    _FC_PROXIES["DeviceInfo"]
                )
            )
            device_info_resp = asyncio.run(hwinfo_device_proxy.get_info())
            return device_info_resp.info
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status

    @property
    def _product_info(self) -> dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            hwinfo_product_proxy = f_hwinfo.Product.Client(
                self.fuchsia_controller.connect_device_proxy(
                    _FC_PROXIES["ProductInfo"]
                )
            )
            product_info_resp = asyncio.run(hwinfo_product_proxy.get_info())
            return product_info_resp.info
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status

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
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            rcs_proxy = fd_remotecontrol.RemoteControl.Client(
                self.fuchsia_controller.ctx.connect_remote_control_proxy()
            )
            asyncio.run(
                rcs_proxy.log_message(
                    tag=tag, message=message, severity=_LOG_SEVERITIES[level]
                )
            )
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status

    def _send_reboot_command(self) -> None:
        """Send a device command to trigger a soft reboot.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            power_proxy = fhp_statecontrol.Admin.Client(
                self.fuchsia_controller.connect_device_proxy(
                    _FC_PROXIES["PowerAdmin"]
                )
            )
            asyncio.run(
                power_proxy.reboot(
                    reason=fhp_statecontrol.RebootReason.USER_REQUEST
                )
            )
        except fcp.ZxStatus as status:
            # ZX_ERR_PEER_CLOSED is expected in this instance because the device
            # powered off.
            zx_status: int | None = (
                status.args[0] if len(status.args) > 0 else None
            )
            if zx_status != fcp.ZxStatus.ZX_ERR_PEER_CLOSED:
                raise errors.FuchsiaControllerError(
                    "Fuchsia Controller FIDL Error"
                ) from status

    def _read_snapshot_from_channel(self, channel_client: fcp.Channel) -> bytes:
        """Read snapshot data from client end of the transfer channel.

        Args:
            channel_client: Client end of the snapshot data channel.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure or on
              data transfer verification failure.

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
        # Snapshot is sent over the channel as |fuchsia.io.File|.
        file_proxy = f_io.File.Client(channel_client)

        # Get file size for verification later.
        try:
            attr_resp: f_io.Node1GetAttrResponse = asyncio.run(
                file_proxy.get_attr()
            )
            if attr_resp.s != fcp.ZxStatus.ZX_OK:
                raise errors.FuchsiaControllerError(
                    f"get_attr() returned status: {attr_resp.s}"
                )
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError("get_attr() failed") from status

        # Read until channel is empty.
        ret: bytearray = bytearray()
        try:
            while True:
                result: f_io.ReadableReadResult = asyncio.run(
                    file_proxy.read(count=f_io.MAX_BUF)
                )
                if result.err:
                    raise errors.FuchsiaControllerError(
                        "read() failed. Received zx.Status {result.err}"
                    )
                if not result.response.data:
                    break
                ret.extend(result.response.data)
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError("read() failed") from status

        # Verify transfer.
        expected_size: int = attr_resp.attributes.content_size
        if len(ret) != expected_size:
            raise errors.FuchsiaControllerError(
                f"Expected {expected_size} bytes, but read {len(ret)} bytes"
            )

        return bytes(ret)

    def _send_snapshot_command(self) -> bytes:
        """Send a device command to take a snapshot.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure or on
              data transfer verification failure.

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
        # Ensure device is healthy and ready to send FIDL requests before
        # sending snapshot command.
        self.fuchsia_controller.create_context()
        self.health_check()

        channel_server, channel_client = fcp.Channel.create()
        params = f_feedback.GetSnapshotParameters(
            # Set timeout to 2 minutes in nanoseconds.
            collection_timeout_per_data=2 * 60 * 10**9,
            response_channel=channel_server.take(),
        )

        try:
            feedback_proxy = f_feedback.DataProvider.Client(
                self.fuchsia_controller.connect_device_proxy(
                    _FC_PROXIES["Feedback"]
                )
            )
            # The data channel isn't populated until get_snapshot() returns so
            # there's no need to drain the channel in parallel.
            asyncio.run(feedback_proxy.get_snapshot(params=params))
        except fcp.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "get_snapshot() failed"
            ) from status
        return self._read_snapshot_from_channel(channel_client)
