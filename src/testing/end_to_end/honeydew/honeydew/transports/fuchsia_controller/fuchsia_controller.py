# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""ABC with methods for Host-(Fuchsia)Target interactions via Fuchsia-Controller."""

import abc

import fuchsia_controller_py as fuchsia_controller

from honeydew.typing import custom_types


class FuchsiaController(abc.ABC):
    """ABC with methods for Host-(Fuchsia)Target interactions via
    Fuchsia-Controller.
    """

    @abc.abstractmethod
    def create_context(self) -> None:
        """Creates the fuchsia-controller context.

        Raises:
            FuchsiaControllerError: Failed to create FuchsiaController Context.
            FuchsiaControllerConnectionError: If target is not ready.
        """

    @abc.abstractmethod
    def check_connection(self) -> None:
        """Checks the Fuchsia-Controller connection from host to Fuchsia device.

        Raises:
            FuchsiaControllerConnectionError
        """

    @abc.abstractmethod
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
