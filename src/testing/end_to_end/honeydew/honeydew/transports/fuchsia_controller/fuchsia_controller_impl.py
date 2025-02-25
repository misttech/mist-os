# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller."""

import logging

import fuchsia_controller_py as fuchsia_controller

from honeydew.transports.ffx import config as ffx_config
from honeydew.transports.fuchsia_controller import errors as fc_errors
from honeydew.transports.fuchsia_controller import (
    fuchsia_controller as fuchsia_controller_interface,
)
from honeydew.typing import custom_types
from honeydew.utils import decorators

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaControllerImpl(fuchsia_controller_interface.FuchsiaController):
    """Provides Host-(Fuchsia)Target interactions via Fuchsia-Controller.

    Args:
        target_name: Fuchsia device name.
        config: Configuration associated with FuchsiaController, FFX and FFX daemon.
        device_ip: Fuchsia device IP Address.

    Raises:
        FuchsiaControllerConnectionError: If target is not ready.
        FuchsiaControllerError: Failed to instantiate.
    """

    def __init__(
        self,
        target_name: str,
        ffx_config_data: ffx_config.FfxConfigData,
        target_ip_port: custom_types.IpPort | None = None,
    ) -> None:
        self._target_name: str = target_name
        self._ffx_config_data: ffx_config.FfxConfigData = ffx_config_data

        self._target_ip_port: custom_types.IpPort | None = target_ip_port

        self._target: str
        if self._target_ip_port:
            self._target = str(self._target_ip_port)
        else:
            self._target = self._target_name

        self.ctx: fuchsia_controller.Context

        self.create_context()
        self.check_connection()

    def create_context(self) -> None:
        """Creates the fuchsia-controller context.

        Raises:
            FuchsiaControllerError: Failed to create FuchsiaController Context.
            FuchsiaControllerConnectionError: If target is not ready.
        """
        try:
            # To run Fuchsia-Controller in isolation
            isolate_dir: fuchsia_controller.IsolateDir | None = (
                self._ffx_config_data.isolate_dir
            )
            config: dict[str, str] = {}
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
            raise fc_errors.FuchsiaControllerError(
                "Failed to create Fuchsia-Controller context"
            ) from err

    @decorators.liveness_check
    def check_connection(self) -> None:
        """Checks the Fuchsia-Controller connection from host to Fuchsia device.

        Raises:
            FuchsiaControllerConnectionError
        """
        try:
            _LOGGER.debug(
                "Waiting for for Fuchsia-Controller to check the "
                "connection from host to %s...",
                self._target_name,
            )
            self.ctx.target_wait(timeout=0)
            _LOGGER.debug(
                "Fuchsia-Controller completed the connection check from host "
                "to %s...",
                self._target_name,
            )
        except Exception as err:  # pylint: disable=broad-except
            raise fc_errors.FuchsiaControllerConnectionError(
                f"Fuchsia-Controller connection check failed for "
                f"{self._target_name} with error: {err}"
            )

    def connect_device_proxy(
        self, fidl_end_point: custom_types.FidlEndpoint
    ) -> fuchsia_controller.Channel:
        """Opens a proxy to the specified FIDL end point.

        Args:
            fidl_end_point: FIDL end point tuple containing moniker and protocol
              name.

        Raises:
            FuchsiaControllerError: On FIDL communication failure.

        Returns:
            FIDL channel to proxy.
        """
        try:
            return self.ctx.connect_device_proxy(
                fidl_end_point.moniker, fidl_end_point.protocol
            )
        except fuchsia_controller.ZxStatus as status:
            raise fc_errors.FuchsiaControllerError(
                "Fuchsia Controller FIDL Error"
            ) from status
