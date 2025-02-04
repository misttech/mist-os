# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy affordance implementation using Fuchsia Controller."""

from __future__ import annotations

import logging

import fidl.fuchsia_net_interfaces as f_net_interfaces
import fidl.fuchsia_net_root as f_net_root
from fuchsia_controller_py import Channel, ZxStatus
from fuchsia_controller_py.wrappers import AsyncAdapter, asyncmethod

from honeydew import errors
from honeydew.affordances.connectivity.netstack import netstack
from honeydew.affordances.connectivity.netstack.errors import (
    HoneydewNetstackError,
)
from honeydew.affordances.connectivity.netstack.types import InterfaceProperties
from honeydew.affordances.connectivity.wlan.utils.types import MacAddress
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.custom_types import FidlEndpoint

# List of required FIDLs for this affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.net.interfaces.State",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)

# Fuchsia Controller proxies
_STATE_PROXY = FidlEndpoint(
    "core/network/netstack", "fuchsia.net.interfaces.State"
)
_INTERFACES_PROXY = FidlEndpoint(
    "core/network/netstack", "fuchsia.net.root.Interfaces"
)


class NetstackUsingFc(AsyncAdapter, netstack.Netstack):
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

        self._connect_proxy()
        self._reboot_affordance.register_for_on_device_boot(self._connect_proxy)

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

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the WLAN stack."""
        self._state_proxy = f_net_interfaces.State.Client(
            self._fc_transport.connect_device_proxy(_STATE_PROXY)
        )
        self._interfaces_proxy = f_net_root.Interfaces.Client(
            self._fc_transport.connect_device_proxy(_INTERFACES_PROXY)
        )

    @asyncmethod
    # pylint: disable-next=invalid-overridden-method
    async def list_interfaces(self) -> list[InterfaceProperties]:
        """List interfaces.

        Returns:
            Information on all interfaces on the device.

        Raises:
            HoneydewNetstackError: Error from the netstack.
            TypeError: Received invalid Watcher events from netstack.
        """
        client, server = Channel.create()
        watcher = f_net_interfaces.Watcher.Client(client.take())

        try:
            self._state_proxy.get_watcher(
                options=f_net_interfaces.WatcherOptions(
                    address_properties_interest=None,
                    include_non_assigned_addresses=None,
                ),
                watcher=server.take(),
            )
        except ZxStatus as status:
            raise HoneydewNetstackError(
                f"State.GetWatcher() error {status}"
            ) from status

        properties: list[InterfaceProperties] = []

        while True:
            try:
                resp = await watcher.watch()
            except ZxStatus as status:
                raise HoneydewNetstackError(
                    f"Watcher.Watch() error {status}"
                ) from status

            event = resp.event
            if event.existing:
                try:
                    res = await self._interfaces_proxy.get_mac(
                        id=event.existing.id
                    )

                    if res.err:
                        _LOGGER.debug(
                            'Failed to find the MAC of interface "%s" (%s); '
                            "it no longer exists",
                            event.existing.name,
                            event.existing.id,
                        )
                        continue  # this is fine and sometimes even expected

                    mac = (
                        MacAddress.from_bytes(bytes(res.response.mac.octets))
                        if res.response.mac
                        else None
                    )
                except ZxStatus as status:
                    raise HoneydewNetstackError(
                        f"Interfaces.GetMac() error {status}"
                    )

                properties.append(
                    InterfaceProperties.from_fidl(event.existing, mac)
                )
            elif event.idle:
                # No more information readily available.
                break
            else:
                raise HoneydewNetstackError(
                    "Received invalid Watcher event from netstack. "
                    f"Expected existing or idle events, got {event}"
                )

        return properties
