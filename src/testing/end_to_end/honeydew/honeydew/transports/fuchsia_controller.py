#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller."""

import logging
from typing import Dict, Optional

import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fuchsia_controller_py as fuchsia_controller

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import ffx as ffx_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)

_FC_PROXIES: Dict[str, custom_types.FidlEndpoint] = {
    "RemoteControl":
        custom_types.FidlEndpoint(
            "/core/remote-control",
            "fuchsia.developer.remotecontrol.RemoteControl"),
}


class FuchsiaController:
    """Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller.

    Args:
        device_name: Fuchsia device name.
    """

    def __init__(self, device_name: str) -> None:
        self._name: str = device_name

        self._ctx: fuchsia_controller.Context
        self.rcs_proxy: fd_remotecontrol.RemoteControl.Client

    def create_context(self) -> None:
        """Creates the fuchsia-controller context and any long-lived proxies.

        Raises:
            errors.FuchsiaControllerError: On FIDL communication failure.
        """
        try:
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
                config=config, isolate_dir=isolate_dir, target=self._name)
        except Exception as err:  # pylint: disable=broad-except
            raise errors.FuchsiaControllerError(
                "Failed to create Fuchsia-Controller context") from err

        try:
            # TODO(fxb/128575): Make connect_remote_control_proxy() work, or
            # remove it.
            self.rcs_proxy = fd_remotecontrol.RemoteControl.Client( \
                    self.connect_device_proxy(_FC_PROXIES["RemoteControl"]))
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Failed to create RemoteControl proxy") from status

    def destroy_context(self) -> None:
        """Destroys the fuchsia-controller context and any long-lived proxies,
        closing the fuchsia-controller connection.
        """
        self._ctx = None
        self.rcs_proxy = None

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
            return self._ctx.connect_device_proxy(
                fidl_end_point.moniker, fidl_end_point.protocol)
        except fuchsia_controller.ZxStatus as status:
            raise errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error") from status
