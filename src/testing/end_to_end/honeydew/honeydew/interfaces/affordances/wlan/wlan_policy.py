#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for wlan policy affordance."""

import abc

from honeydew.typing.wlan import ClientStateSummary
from honeydew.typing.wlan import NetworkConfig
from honeydew.typing.wlan import SecurityType


class WlanPolicy(abc.ABC):
    """Abstract base class for WlanPolicy affordance."""

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def create_client_controller(self) -> None:
        """Initializes the client controller."""

    @abc.abstractmethod
    def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device."""

    @abc.abstractmethod
    def get_update(self) -> ClientStateSummary:
        """Gets one client listener update.

        Returns:
            An update of connection status. If there is no error, the result is
            a WlanPolicyUpdate with a structure that matches the FIDL
            ClientStateSummary struct given for updates.
        """

    @abc.abstractmethod
    def save_network(
            self,
            target_ssid: str,
            security_type: SecurityType,
            target_pwd: str | None = None) -> None:
        """Saves a network to the device.

        Args:
            target_ssid: The network to save.
            security_type: The security protocol of the network.
            target_pwd: The credential being saved with the network. No password
                is equivalent to an empty string.
        """

    @abc.abstractmethod
    def remove_all_networks(self) -> None:
        """Deletes all saved networks on the device."""

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
