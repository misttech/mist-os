# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for wlan affordance."""

import abc

from honeydew.affordances import affordance
from honeydew.affordances.connectivity.wlan.utils.types import (
    Authentication,
    BssDescription,
    ClientStatusResponse,
    CountryCode,
    QueryIfaceResponse,
    WlanMacRole,
)


class Wlan(affordance.Affordance):
    """Abstract base class for Wlan driver affordance."""

    # List all the public methods
    @abc.abstractmethod
    def connect(
        self,
        ssid: str,
        # TODO(http://b/356234331): Remove the password field once
        # authentication is used everywhere.
        password: str | None,
        bss_desc: BssDescription,
        authentication: Authentication | None = None,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network. Deprecated; use
                authentication instead.
            bss_desc: The basic service set for target network.
            authentication: Authentication to connect with.

        Returns:
            True on success otherwise false.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
        """

    @abc.abstractmethod
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

    @abc.abstractmethod
    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def disconnect(self) -> None:
        """Disconnect all client WLAN connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.

        Raises:
            HoneydewWlanError: DeviceMonitor.QueryIface error
        """

    @abc.abstractmethod
    def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
        """

    @abc.abstractmethod
    def set_region(self, region_code: CountryCode) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            TypeError: Invalid region_code format
        """

    @abc.abstractmethod
    def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            An implementation of the ClientStatusResponse protocol:
            ClientStatusConnected, ClientStatusConnecting, or ClientStatusIdle.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
        """
