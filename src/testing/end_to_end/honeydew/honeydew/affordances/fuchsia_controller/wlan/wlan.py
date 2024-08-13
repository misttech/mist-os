# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN affordance implementation using Fuchsia Controller."""

import logging

from honeydew import errors
from honeydew.interfaces.affordances.wlan import wlan
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import (
    BssDescription,
    ClientStatusResponse,
    CountryCode,
    QueryIfaceResponse,
    WlanMacRole,
)

# List of required FIDLs for the WLAN Fuchsia Controller affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.location.namedplace",
    "fuchsia.wlan.device.service",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Wlan(wlan.Wlan):
    """WLAN affordance implemented with Fuchsia Controller."""

    def __init__(
        self,
        device_name: str,
        ffx: ffx_transport.FFX,
        fuchsia_controller: fc_transport.FuchsiaController,
        reboot_affordance: affordances_capable.RebootCapableDevice,
    ) -> None:
        """Create a WLAN Fuchsia Controller affordance.

        Args:
            device_name: Device name returned by `ffx target list`.
            ffx: FFX transport.
            fuchsia_controller: Fuchsia Controller transport.
            reboot_affordance: Object that implements RebootCapableDevice.
        """
        self._verify_supported(device_name, ffx)

        self._fc_transport = fuchsia_controller
        self._reboot_affordance = reboot_affordance

    def _verify_supported(self, device: str, ffx: ffx_transport.FFX) -> None:
        """Check if WLAN is supported on the DUT.

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
                    "All available WLAN component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.wlan"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def connect(
        self,
        ssid: str,
        password: str | None,
        bss_desc: BssDescription,
        # TODO(http://b/324949922): Uncomment once the WLAN affordance API is
        # changed to include the desired authentication in every call to
        # connect().
        # authentication: Authentication,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network.
            bss_desc: The basic service set for target network.

        Returns:
            True on success otherwise false.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            InterfaceNotFoundError: No client WLAN interface found.
        """
        raise NotImplementedError()

    def create_iface(
        self, phy_id: int, role: WlanMacRole, sta_addr: str | None = None
    ) -> int:
        """Create a new WLAN interface.

        Args:
            phy_id: The iface ID.
            role: The role of the new iface.
            sta_addr: MAC address for softAP iface.

        Returns:
            Iface id of newly created interface.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            ValueError: Invalid MAC address
        """
        raise NotImplementedError()

    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        raise NotImplementedError()

    def disconnect(self) -> None:
        """Disconnect all client WLAN connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        raise NotImplementedError()

    def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        raise NotImplementedError()

    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        raise NotImplementedError()

    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """
        raise NotImplementedError()

    def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error
        """
        raise NotImplementedError()

    def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            HoneydewWlanError: Error from WLAN stack
            InterfaceNotFoundError: No client WLAN interface found.
        """
        raise NotImplementedError()

    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            TypeError: Invalid region_code format
        """
        raise NotImplementedError()

    def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            ClientStatusResponse which can be any one of three  things:
            ClientStatusConnected, ClientStatusConnecting, ClientStatusIdle.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            InterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
        """
        raise NotImplementedError()
