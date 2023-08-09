#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FuchsiaDevice abstract base class implementation using Fuchsia-Controller."""

import asyncio
import collections
import logging
from typing import Any, Dict, Optional

import fidl.fuchsia_buildinfo as f_buildinfo
import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fidl.fuchsia_diagnostics as f_diagnostics
import fidl.fuchsia_hardware_power_statecontrol as fhp_statecontrol
import fidl.fuchsia_hwinfo as f_hwinfo
import fuchsia_controller_py as fuchsia_controller

from honeydew import custom_types
from honeydew import errors
from honeydew.affordances.fuchsia_controller import component as component_fc
from honeydew.affordances.fuchsia_controller import tracing as tracing_fc
from honeydew.affordances.fuchsia_controller.bluetooth import \
    bluetooth_gap as bluetooth_gap_fc
from honeydew.device_classes import base_fuchsia_device
from honeydew.interfaces.affordances import component
from honeydew.interfaces.affordances import tracing
from honeydew.interfaces.affordances.bluetooth import \
    bluetooth_gap as bluetooth_gap_interface
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.utils import properties

_FidlEndpoint = collections.namedtuple("_FidlEndpoint", ["moniker", "protocol"])
_FC_PROXIES: Dict[str, _FidlEndpoint] = {
    "BuildInfo":
        _FidlEndpoint("/core/build-info", "fuchsia.buildinfo.Provider"),
    "DeviceInfo":
        _FidlEndpoint("/core/hwinfo", "fuchsia.hwinfo.Device"),
    "ProductInfo":
        _FidlEndpoint("/core/hwinfo", "fuchsia.hwinfo.Product"),
    "PowerAdmin":
        _FidlEndpoint(
            "/bootstrap/shutdown_shim",
            "fuchsia.hardware.power.statecontrol.Admin"),
    "RemoteControl":
        _FidlEndpoint(
            "/core/remote-control",
            "fuchsia.developer.remotecontrol.RemoteControl"),
}

_LOG_SEVERITIES: Dict[custom_types.LEVEL, f_diagnostics.Severity] = {
    custom_types.LEVEL.INFO: f_diagnostics.Severity.INFO,
    custom_types.LEVEL.WARNING: f_diagnostics.Severity.WARN,
    custom_types.LEVEL.ERROR: f_diagnostics.Severity.ERROR,
}

_LOGGER: logging.Logger = logging.getLogger(__name__)


def _connect_device_proxy(
        ctx: fuchsia_controller.Context,
        proxy_name: str) -> fuchsia_controller.FidlChannel:
    """Opens a proxy to the device, according to a lookup table of names.

    Args:
        proxy_name: Name of the lookup table entry to use for the proxy's
        moniker and protocol name.

    Raises:
        errors.FuchsiaControllerError: On FIDL communication failure.

    Returns:
        FIDL channel to proxy.
    """
    try:
        return ctx.connect_device_proxy(
            _FC_PROXIES[proxy_name].moniker, _FC_PROXIES[proxy_name].protocol)
    except fuchsia_controller.ZxStatus as status:
        raise errors.FuchsiaControllerError(
            "Fuchsia Controller FIDL Error") from status


class FuchsiaDevice(base_fuchsia_device.BaseFuchsiaDevice,
                    affordances_capable.BluetoothGapCapableDevice,
                    affordances_capable.ComponentCapableDevice,
                    affordances_capable.TracingCapableDevice):
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
        self._ctx: fuchsia_controller.Context
        self._rcs_proxy: fd_remotecontrol.RemoteControl.Client
        super().__init__(device_name, ssh_private_key, ssh_user)

        _LOGGER.debug("Initializing FC-based FuchsiaDevice")
        self._context_create()

    # List all the affordances in alphabetical order
    @properties.Affordance
    def bluetooth_gap(self) -> bluetooth_gap_interface.BluetoothGap:
        """Returns a BluetoothGap affordance object.

        Returns:
            bluetooth_gap.BluetoothGap object
        """
        return bluetooth_gap_fc.BluetoothGap()

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
        # Explicitly destroy the context to close the fuchsia controller
        # connection.
        self._context_destroy()

    def on_device_boot(self) -> None:
        """Take actions after the device is rebooted.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        # Create a new Fuchsia controller context for new device connection.
        self._context_create()

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
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            buildinfo_provider_proxy = f_buildinfo.Provider.Client(
                _connect_device_proxy(self._ctx, "BuildInfo"))
            build_info_resp = asyncio.run(
                buildinfo_provider_proxy.get_build_info())
            return build_info_resp.build_info
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error") from status

    @property
    def _device_info(self) -> Dict[str, Any]:
        """Returns the device information of the device.

        Returns:
            Device info dict.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            hwinfo_device_proxy = f_hwinfo.Device.Client(
                _connect_device_proxy(self._ctx, "DeviceInfo"))
            device_info_resp = asyncio.run(hwinfo_device_proxy.get_info())
            return device_info_resp.info
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error") from status

    @property
    def _product_info(self) -> Dict[str, Any]:
        """Returns the product information of the device.

        Returns:
            Product info dict.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            hwinfo_product_proxy = f_hwinfo.Product.Client(
                _connect_device_proxy(self._ctx, "ProductInfo"))
            product_info_resp = asyncio.run(hwinfo_product_proxy.get_info())
            return product_info_resp.info
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error") from status

    # List all private methods in alphabetical order
    def _context_create(self) -> None:
        """Creates the fuchsia-controller context and any long-lived proxies.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            target: str = self.device_name

            ffx_config: custom_types.FFXConfig = ffx_transport.get_config()

            # To run Fuchsia-Controller in isolation
            isolate_dir: Optional[fuchsia_controller.IsolateDir] = \
                ffx_config.isolate_dir

            # To collect Fuchsia-Controller logs
            config: Dict[str, str] = {}
            if ffx_config.logs_dir:
                config["log.dir"] = ffx_config.logs_dir
                config["log.level"] = "debug"

            # Do not autostart the daemon if it is not running.
            # If Fuchsia-Controller need to start a daemon then it needs to know
            # SDK path to find FFX CLI.
            # However, HoneyDew calls FFX CLI (and thus starts the FFX daemon)
            # even before it instantiates Fuchsia-Controller. So tell
            # Fuchsia-Controller to use the same daemon (by pointing to same
            # isolate-dir and logs-dir path used to start the daemon) and set
            # "daemon.autostart" to "false".
            config["daemon.autostart"] = "false"

            # Overnet, the legacy implementation has known issues and is
            # deprecated, but at the moment it is still active by default, for
            # compatibility reasons. It should be disabled when those
            # compatibility concerns are not relevant. "overnet.cso" disables
            # legacy code in favor of its modern replacement,
            # CSO (Circuit-Switched Overnet).
            config["overnet.cso"] = "only"

            self._ctx = fuchsia_controller.Context(
                config=config, isolate_dir=isolate_dir, target=target)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaControllerError(
                "Failed to create Fuchsia-Controller context") from err

        try:
            # TODO(fxb/128575): Make connect_remote_control_proxy() work, or
            # remove it.
            self._rcs_proxy = fd_remotecontrol.RemoteControl.Client( \
                    _connect_device_proxy(self._ctx, "RemoteControl"))
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Failed to create RemoteControl proxy") from status

    def _context_destroy(self) -> None:
        """Destroys the fuchsia-controller context and any long-lived proxies,
        closing the fuchsia-controller connection.
        """
        self._ctx = None
        self._rcs_proxy = None

    def _send_log_command(
            self, tag: str, message: str, level: custom_types.LEVEL) -> None:
        """Send a device command to write to the syslog.

        Args:
            tag: Tag to apply to the message in the syslog.
            message: Message that need to logged.
            level: Log message level.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            asyncio.run(
                self._rcs_proxy.log_message(
                    tag=tag, message=message, severity=_LOG_SEVERITIES[level]))
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error") from status

    def _send_reboot_command(self) -> None:
        """Send a device command to trigger a soft reboot.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
            power_proxy = fhp_statecontrol.Admin.Client(
                _connect_device_proxy(self._ctx, "PowerAdmin"))
            asyncio.run(
                power_proxy.reboot(
                    reason=fhp_statecontrol.RebootReason.USER_REQUEST))
        except fuchsia_controller.ZxStatus as status:
            # ZX_ERR_PEER_CLOSED is expected in this instance because the device
            # powered off.
            zx_status: Optional[int] = \
                status.args[0] if len(status.args) > 0 else None
            if zx_status != fuchsia_controller.ZxStatus.ZX_ERR_PEER_CLOSED:
                raise errors.FuchsiaControllerError(
                    "Fuchsia Controller FIDL Error") from status

    def _send_snapshot_command(self) -> bytes:
        """Send a device command to take a snapshot.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.

        Returns:
            Bytes containing snapshot data as a zip archive.
        """
        # TODO(b/286052015): Implement snapshot via `response_channel` field in
        # `GetSnapshot`, and reading a `fuchsia.io.File` channel. See the
        # implementation of the `ffx target snapshot` command.
        raise NotImplementedError
