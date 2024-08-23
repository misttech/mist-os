# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""WLAN policy affordance implementation using Fuchsia Controller."""

from __future__ import annotations

import logging

from honeydew import errors
from honeydew.interfaces.affordances.wlan import wlan_policy
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.transports import ffx as ffx_transport
from honeydew.interfaces.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import (
    ClientStateSummary,
    NetworkConfig,
    RequestStatus,
    SecurityType,
)

# List of required FIDLs for the WLAN Fuchsia Controller affordance.
_REQUIRED_CAPABILITIES = [
    "fuchsia.wlan.policy.ClientListener",
    "fuchsia.wlan.policy.ClientProvider",
    "fuchsia.wlan.phyimpl",
]

_LOGGER: logging.Logger = logging.getLogger(__name__)


class WlanPolicy(wlan_policy.WlanPolicy):
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
                    "All available WLAN component capabilities:\n%s",
                    ffx.run(["component", "capability", "fuchsia.wlan"]),
                )
                raise errors.NotSupportedError(
                    f'Component capability "{capability}" not exposed by device '
                    f"{device}; this build of Fuchsia does not support the "
                    "WLAN FC affordance."
                )

    def _connect_proxy(self) -> None:
        """Re-initializes connection to the WLAN stack."""

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

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return value not a string.
        """
        raise NotImplementedError()

    def create_client_controller(self) -> None:
        """Initializes the client controller.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

    def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device.

        Returns:
            A list of NetworkConfigs.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return values not correct types.
        """
        raise NotImplementedError()

    def get_update(
        self,
        timeout: float | None = None,
    ) -> ClientStateSummary:
        """Gets one client listener update.

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
            An update of connection status. If there is no error, the result is
            a WlanPolicyUpdate with a structure that matches the FIDL
            ClientStateSummary struct given for updates.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return values not correct types.
        """
        raise NotImplementedError()

    def remove_all_networks(self) -> None:
        """Deletes all saved networks on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

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

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

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

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

    def scan_for_networks(self) -> list[str]:
        """Scans for networks.

        Returns:
            A list of network SSIDs that can be connected to.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return value not a list.
        """
        raise NotImplementedError()

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.

        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

    def start_client_connections(self) -> None:
        """Enables device to initiate connections to networks.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()

    def stop_client_connections(self) -> None:
        """Disables device for initiating connections to networks.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        raise NotImplementedError()
