#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for wlan affordance."""

import abc

from honeydew.typing.wlan import (
    BssDescription,
    ClientStatusResponse,
    CountryCode,
    QueryIfaceResponse,
    WlanMacRole,
)


class Wlan(abc.ABC):
    """Abstract base class for Wlan driver affordance."""

    # List all the public methods
    @abc.abstractmethod
    def connect(
        self,
        ssid: str,
        password: str | None,
        bss_desc: BssDescription,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network.
            bss_desc: The basic service set for target network.

        Returns:
            True on success otherwise false.
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
        """

    @abc.abstractmethod
    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.
        """

    @abc.abstractmethod
    def disconnect(self) -> None:
        """Disconnect any current wifi connections."""

    @abc.abstractmethod
    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.
        """

    @abc.abstractmethod
    def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.
        """

    @abc.abstractmethod
    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy IDs that are present on the device.
        """

    @abc.abstractmethod
    def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.
        """

    @abc.abstractmethod
    def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network
        """

    @abc.abstractmethod
    def set_region(self, region_code: CountryCode) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.
        """

    @abc.abstractmethod
    def status(self) -> ClientStatusResponse:
        """Request client state and network status.

        Returns:
            ClientStatusResponse state summary and
            status of various networks connections.
        """
