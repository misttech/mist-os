# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for WLAN Policy Access Point affordance."""

import abc

from honeydew.affordances import affordance
from honeydew.affordances.connectivity.wlan.utils.types import (
    AccessPointState,
    ConnectivityMode,
    OperatingBand,
    SecurityType,
)


class WlanPolicyAp(affordance.Affordance):
    """Abstract base class for the WLAN Policy Access Point affordance."""

    # List all the public methods
    @abc.abstractmethod
    def start(
        self,
        ssid: str,
        security: SecurityType,
        password: str | None,
        mode: ConnectivityMode,
        band: OperatingBand,
    ) -> None:
        """Start an access point.

        Args:
            ssid: SSID of the network to start.
            security: The security protocol of the network.
            password: Credential used to connect to the network. None is
                equivalent to no password.
            mode: The connectivity mode to use
            band: The operating band to use

        Raises:
            HoneydewWlanError: Error from WLAN stack
            HoneydewWlanRequestRejectedError: WLAN rejected this request
        """

    @abc.abstractmethod
    def stop(
        self,
        ssid: str,
        security: SecurityType,
        password: str | None,
    ) -> None:
        """Stop an active access point.

        Args:
            ssid: SSID of the network to stop.
            security: The security protocol of the network.
            password: Credential used to connect to the network. None is
                equivalent to no password.

        Raises:
            HoneydewWlanError: Error from WLAN stack
            HoneydewWlanRequestRejectedError: WLAN rejected this request
        """

    @abc.abstractmethod
    def stop_all(self) -> None:
        """Stop all active access points.

        Raises:
            HoneydewWlanError: Error from WLAN stack
        """

    @abc.abstractmethod
    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """

    @abc.abstractmethod
    def get_update(
        self,
        timeout: float | None = None,
    ) -> list[AccessPointState]:
        """Get a list of AP state listener updates.

        This call will return with an update immediately the
        first time the update listener is initialized by setting a new listener
        or by creating a client controller before setting a new listener.
        Subsequent calls will hang until there is a change since the last
        update call.

        Args:
            timeout: Timeout in seconds to wait for the get_update command to
                return. By default it is set to None (which means timeout is
                disabled)

        Returns:
            A list of AP state updates.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TimeoutError: Reached timeout without any updates.
        """
