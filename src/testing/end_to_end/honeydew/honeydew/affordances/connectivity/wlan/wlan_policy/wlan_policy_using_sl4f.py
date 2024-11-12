# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan policy affordance implementation using SL4F."""

from collections.abc import Mapping
from enum import StrEnum

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    ClientStateSummary,
    ConnectionState,
    DisconnectStatus,
    NetworkConfig,
    NetworkIdentifier,
    NetworkState,
    RequestStatus,
    SecurityType,
    WlanClientState,
)
from honeydew.affordances.connectivity.wlan.wlan_policy import wlan_policy
from honeydew.transports.sl4f import SL4F


def _get_str(m: Mapping[str, object], key: str) -> str:
    val = m.get(key)
    if not isinstance(val, str):
        raise TypeError(f'Expected "{val}" to be str, got {type(val)}')
    return val


class _Sl4fMethods(StrEnum):
    """Sl4f server commands."""

    CONNECT = "wlan_policy.connect"
    CREATE_CLIENT_CONTROLLER = "wlan_policy.create_client_controller"
    GET_SAVED_NETWORKS = "wlan_policy.get_saved_networks"
    GET_UPDATE = "wlan_policy.get_update"
    REMOVE_ALL_NETWORKS = "wlan_policy.remove_all_networks"
    REMOVE_NETWORK = "wlan_policy.remove_network"
    SAVE_NETWORK = "wlan_policy.save_network"
    SCAN_FOR_NETWORKS = "wlan_policy.scan_for_networks"
    SET_NEW_UPDATE_LISTENER = "wlan_policy.set_new_update_listener"
    START_CLIENT_CONNECTIONS = "wlan_policy.start_client_connections"
    STOP_CLIENT_CONNECTIONS = "wlan_policy.stop_client_connections"


class WlanPolicy(wlan_policy.WlanPolicy):
    """WlanPolicy affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """

    def __init__(self, device_name: str, sl4f: SL4F) -> None:
        self._name: str = device_name
        self._sl4f: SL4F = sl4f

        self.verify_supported()

    # List all the public methods
    def verify_supported(self) -> None:
        """Verifies that the WlanPolicy affordance using SL4F is supported by the Fuchsia device.

        This method should be called in `__init__()` so that if this affordance was called on a
        Fuchsia device that does not support it, it will raise NotSupportedError.

        Raises:
            NotSupportedError: If affordance is not supported.
        """
        # TODO(http://b/377585939): To Be Implemented
        return

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
        method_params = {
            "target_ssid": target_ssid,
            "security_type": str(security_type),
        }
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.CONNECT, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to connect") from e
        result: object = resp.get("result")

        if not isinstance(result, str):
            raise TypeError(f'Expected "result" to be str, got {type(result)}')

        return RequestStatus(result)

    def create_client_controller(self) -> None:
        """Initializes the client controller.

        See fuchsia.wlan.policy/ClientProvider.GetController().

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.CREATE_CLIENT_CONTROLLER)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to create_client_controller"
            ) from e

    def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device.

        Returns:
            A list of NetworkConfigs.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return values not correct types.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.GET_SAVED_NETWORKS
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to get_saved_networks"
            ) from e
        result: object = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        networks: list[NetworkConfig] = []
        for n in result:
            if not isinstance(n, dict):
                raise TypeError(f'Expected "network" to be dict, got {type(n)}')

            security_type = _get_str(n, "security_type")
            networks.append(
                NetworkConfig(
                    ssid=_get_str(n, "ssid"),
                    security_type=SecurityType(security_type.lower()),
                    credential_type=_get_str(n, "credential_type"),
                    credential_value=_get_str(n, "credential_value"),
                )
            )

        return networks

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
            TimeoutError: Reached timeout without any updates.
            TypeError: Return values not correct types.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.GET_UPDATE,
                attempts=1,
                timeout=timeout,
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to get_update") from e
        result: object = resp.get("result")

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        networks = result.get("networks")
        if not isinstance(networks, list):
            raise TypeError(
                'Expected "networks" to be list, '
                f'got {type(result.get("networks"))}'
            )

        network_states: list[NetworkState] = []
        for network in networks:
            state: str | None = network.get("state")
            status: str | None = network.get("status")
            if state is None:
                state = ConnectionState.DISCONNECTED
            if status is None:
                status = DisconnectStatus.CONNECTION_STOPPED

            net_id = network.get("id")
            ssid: str = net_id.get("ssid")
            security_type: str = net_id.get("type_")

            network_states.append(
                NetworkState(
                    network_identifier=NetworkIdentifier(
                        ssid=ssid,
                        security_type=SecurityType(security_type.lower()),
                    ),
                    connection_state=ConnectionState(state),
                    disconnect_status=DisconnectStatus(status),
                )
            )

        return ClientStateSummary(
            state=WlanClientState(result.get("state", None)),
            networks=network_states,
        )

    def remove_all_networks(self) -> None:
        """Deletes all saved networks on the device.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.REMOVE_ALL_NETWORKS)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to remove_all_networks"
            ) from e

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
        if not target_pwd:
            target_pwd = ""

        method_params = {
            "target_ssid": target_ssid,
            "security_type": str(security_type),
            "target_pwd": target_pwd,
        }
        try:
            self._sl4f.run(
                method=_Sl4fMethods.REMOVE_NETWORK, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to remove_network"
            ) from e

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
        if not target_pwd:
            target_pwd = ""

        method_params: dict[str, object] = {
            "target_ssid": target_ssid,
            "security_type": str(security_type.value),
            "target_pwd": target_pwd,
        }
        try:
            self._sl4f.run(
                method=_Sl4fMethods.SAVE_NETWORK, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to save_network") from e

    def scan_for_networks(self) -> list[str]:
        """Scans for networks.

        Returns:
            A list of network SSIDs that can be connected to.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            TypeError: Return value not a list.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.SCAN_FOR_NETWORKS
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to scan_for_networks"
            ) from e
        result: object = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        return result

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.
        This causes updates to be reset. Intended to be used between tests so
        that the behavior of updates in a test is independent from previous
        tests.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.SET_NEW_UPDATE_LISTENER)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to set_new_update_listener"
            ) from e

    def start_client_connections(self) -> None:
        """Enables device to initiate connections to networks.

        Either by auto-connecting to saved networks or acting on incoming calls
        triggering connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.START_CLIENT_CONNECTIONS)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to start_client_connections"
            ) from e

    def stop_client_connections(self) -> None:
        """Disables device for initiating connections to networks.

        Tears down any existing connections to WLAN networks and disables
        initiation of new connections.

        Raises:
            HoneydewWlanError: Error from WLAN stack.
            RuntimeError: A client controller has not been created yet
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.STOP_CLIENT_CONNECTIONS)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to stop_client_connections"
            ) from e
