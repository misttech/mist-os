#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller."""

import ipaddress
import logging

import fuchsia_controller_py as fuchsia_controller

from honeydew import custom_types, errors
from honeydew.transports import ffx

_LOGGER: logging.Logger = logging.getLogger(__name__)

_TIMEOUTS: dict[str, float] = {
    "TARGET_WAIT": 15,
}


class FuchsiaController:
    """Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller.

    Args:
        device_name: Fuchsia device name.
        ffx_transport: ffx.FFX object.
        device_ip: Fuchsia device IP Address.
    """

    def __init__(
        self,
        device_name: str,
        ffx_transport: ffx.FFX,
        device_ip: ipaddress.IPv4Address | ipaddress.IPv6Address | None = None,
    ) -> None:
        self._name: str = device_name

        self._ip_address: ipaddress.IPv4Address | ipaddress.IPv6Address | None = (
            device_ip
        )
        self._target: str
        if self._ip_address:
            self._target = str(self._ip_address)
        else:
            self._target = self._name

        self._ffx_transport: ffx.FFX = ffx_transport

        self.ctx: fuchsia_controller.Context
        self.create_context()

    def create_context(self) -> None:
        """Creates the fuchsia-controller context and waits for the target to
        be ready.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
            errors.FuchsiaControllerConnectionError: If target is not ready.
        """
        try:
            ffx_config: custom_types.FFXConfig = self._ffx_transport.config

            # To run Fuchsia-Controller in isolation
            isolate_dir: fuchsia_controller.IsolateDir | None = (
                ffx_config.isolate_dir
            )

            # To collect Fuchsia-Controller logs
            config: dict[str, str] = {}
            if ffx_config.logs_dir:
                config["log.dir"] = ffx_config.logs_dir
                config["log.level"] = ffx_config.logs_level

            # Do not autostart the daemon if it is not running.
            # If Fuchsia-Controller need to start a daemon then it needs to know
            # SDK path to find FFX CLI.
            # However, Honeydew calls FFX CLI (and thus starts the FFX daemon)
            # even before it instantiates Fuchsia-Controller. So tell
            # Fuchsia-Controller to use the same daemon (by pointing to same
            # isolate-dir and logs-dir path used to start the daemon) and set
            # "daemon.autostart" to "false".
            config["daemon.autostart"] = "false"

            msg: str = (
                f"Creating Fuchsia-Controller Context with "
                f"target='{self._target}', config='{config}'"
            )
            if isolate_dir:
                msg = f"{msg}, isolate_dir={isolate_dir.directory()}"
            _LOGGER.debug(msg)
            self.ctx = fuchsia_controller.Context(
                config=config, isolate_dir=isolate_dir, target=self._target
            )
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaControllerError(
                "Failed to create Fuchsia-Controller context"
            ) from err

        self.check_connection()

    def check_connection(
        self, timeout: float = _TIMEOUTS["TARGET_WAIT"]
    ) -> None:
        """Checks the Fuchsia-Controller connection from host to Fuchsia device.

        Args:
            timeout: How long in seconds to wait.

        Raises:
            errors.FuchsiaControllerConnectionError
        """
        try:
            _LOGGER.debug(
                "Waiting for %s sec for Fuchsia-Controller to check the "
                "connection from host to %s...",
                timeout,
                self._name,
            )
            self.ctx.target_wait(timeout)
            _LOGGER.debug(
                "Fuchsia-Controller completed the connection check from host "
                "to %s...",
                self._name,
            )
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaControllerConnectionError(
                f"Fuchsia-Controller connection check failed for {self._name} "
                f"with error: {err}"
            )

    def connect_device_proxy(
        self, fidl_end_point: custom_types.FidlEndpoint
    ) -> fuchsia_controller.Channel:
        """Opens a proxy to the specified FIDL end point.

        Args:
            fidl_end_point: FIDL end point tuple containing moniker and protocol
              name.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.

        Returns:
            FIDL channel to proxy.
        """
        try:
            return self.ctx.connect_device_proxy(
                fidl_end_point.moniker, fidl_end_point.protocol
            )
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status
