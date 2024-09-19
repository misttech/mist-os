# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy affordance implementation using Fuchsia Controller."""

from __future__ import annotations

import logging

from honeydew import errors
from honeydew.interfaces.affordances import netstack
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.netstack import InterfaceProperties

# List of required FIDLs for this affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.net.interfaces.State",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Netstack(netstack.Netstack):
    """WLAN affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Create a WLAN Policy Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
        """
        super().__init__()
        self._verify_supported(device_name, ffx)

        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance

    def _verify_supported(self, device: str, ffx: ffx_transport.FFX) -> None:
        """Check if WLAN Policy is supported on the DUT.

        Args:
            device: Device name returned by `ffx target list`.
            ffx: FFX transport

        Raises:
            NotSupportedError: A required component capability is not available.
        """
        for capability in _REQUIRED_CAPABILITIES:
            # TODO(http://b/359342196): This is a maintenance burden; find a
            # better way to detect FIDL component capabilities.
            if capability not in ffx.run(
                ["component", "capability", capability]
            ):
                _LOGGER.warning(
                    "All available netstack component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.net"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def list_interfaces(self) -> list[InterfaceProperties]:
        """List interfaces.

        Returns:
            Information on all interfaces on the device.

        Raises:
            HoneydewNetstackError: Error from the netstack.
        """
        raise NotImplementedError()
