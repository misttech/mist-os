#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for wlan policy affordance."""

import abc

from honeydew.typing.wlan import (
    ClientStateSummary,
    NetworkConfig,
    RequestStatus,
    SecurityType,
)

DEFAULTS: dict[str, float] = {
    "UPDATE_TIMEOUT_S": 30.0,
}


class WlanPolicy(abc.ABC):
    """Abstract base class for WlanPolicy affordance."""

    # List all the public methods
    @abc.abstractmethod
    def connect(
        self, target_ssid: str, security_type: SecurityType
    ) -> RequestStatus:
        """Triggers connection to a network.

        Args:
            target_ssid: The network to connect to. Must have been previously
                saved in order for a successful connection to happen.
            security_type: The security protocol of the network.

        Returns:
            A RequestStatus response to the connect request
        """

    @abc.abstractmethod
    def create_client_controller(self) -> None:
        """Initializes the client controller."""

    @abc.abstractmethod
    def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device.

        Returns:
            A list of NetworkConfigs.
        """

    @abc.abstractmethod
    def get_update(
        self, timeout: float = DEFAULTS["UPDATE_TIMEOUT_S"]
    ) -> ClientStateSummary:
        """Gets one client listener update.

        This call will return with an update immediately the
        first time the update listener is initialized by setting a new listener
        or by creating a client controller before setting a new listener.
        Subsequent calls will hang until there is a change since the last
        update call.

        Args:
            timeout: Timeout in seconds to wait for the get_update command to
                return.

        Returns: ClientStateSummary
        """

    @abc.abstractmethod
    def remove_all_networks(self) -> None:
        """Deletes all saved networks on the device."""

    @abc.abstractmethod
    def remove_network(
        self,
        target_ssid: str,
        security_type: SecurityType,
        target_pwd: str | None = None,
    ) -> None:
        """Removes or "forgets" a network from saved networks.

        Args:
            target_ssid: The network to remove.
            security_type: The security protocol of the network.
            target_pwd: The credential being saved with the network. No password
                is equivalent to an empty string.
        """

    @abc.abstractmethod
    def save_network(
        self,
        target_ssid: str,
        security_type: SecurityType,
        target_pwd: str | None = None,
    ) -> None:
        """Saves a network to the device.

        Args:
            target_ssid: The network to save.
            security_type: The security protocol of the network.
            target_pwd: The credential being saved with the network. No password
                is equivalent to an empty string.
        """

    @abc.abstractmethod
    def scan_for_networks(self) -> list[str]:
        """Scans for networks.

        Returns:
            A list of network SSIDs that can be connected to.
        """

    @abc.abstractmethod
    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behaviour of updates in a test is independent from previous
        tests.
        """

    @abc.abstractmethod
    def start_client_connections(self) -> None:
        """Enables device to initiate connections to networks."""

    @abc.abstractmethod
    def stop_client_connections(self) -> None:
        """Disables device for initiating connections to networks."""
