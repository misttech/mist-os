#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan policy affordance implementation using SL4F."""

from collections.abc import Mapping
from enum import StrEnum

from honeydew.interfaces.affordances.wlan import wlan_policy
from honeydew.transports.sl4f import SL4F
from honeydew.typing.wlan import (
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

    # List all the public methods
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
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return value not a string.
        """
        method_params = {
            "target_ssid": target_ssid,
            "security_type": str(security_type),
        }
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.CONNECT, params=method_params
        )
        result: object = resp.get("result")

        if not isinstance(result, str):
            raise TypeError(f'Expected "result" to be str, got {type(result)}')

        return RequestStatus(result)

    def create_client_controller(self) -> None:
        """Initializes the client controller.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """

        self._sl4f.run(method=_Sl4fMethods.CREATE_CLIENT_CONTROLLER)

    def get_saved_networks(self) -> list[NetworkConfig]:
        """Gets networks saved on device.

        Returns:
            A list of NetworkConfigs.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return values not correct types.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.GET_SAVED_NETWORKS
        )
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
        self, timeout: float = wlan_policy.DEFAULTS["UPDATE_TIMEOUT_S"]
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

        Returns:
            An update of connection status. If there is no error, the result is
            a WlanPolicyUpdate with a structure that matches the FIDL
            ClientStateSummary struct given for updates.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return values not correct types.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.GET_UPDATE, timeout=timeout
        )
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
            errors.Sl4fError: Sl4f run command failed.
        """
        self._sl4f.run(method=_Sl4fMethods.REMOVE_ALL_NETWORKS)

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
            errors.Sl4fError: Sl4f run command failed.
        """
        if not target_pwd:
            target_pwd = ""

        method_params = {
            "target_ssid": target_ssid,
            "security_type": str(security_type),
            "target_pwd": target_pwd,
        }
        self._sl4f.run(method=_Sl4fMethods.REMOVE_NETWORK, params=method_params)

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
            errors.Sl4fError: Sl4f run command failed.
        """
        if not target_pwd:
            target_pwd = ""

        method_params: dict[str, object] = {
            "target_ssid": target_ssid,
            "security_type": str(security_type.value),
            "target_pwd": target_pwd,
        }
        self._sl4f.run(method=_Sl4fMethods.SAVE_NETWORK, params=method_params)

    def scan_for_networks(self) -> list[str]:
        """Scans for networks.

        Returns:
            A list of network SSIDs that can be connected to.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return value not a list.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.SCAN_FOR_NETWORKS
        )
        result: object = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        return result

    def set_new_update_listener(self) -> None:
        """Sets the update listener stream of the facade to a new stream.
        This causes updates to be reset. Intended to be used between tests so
        that the behaviour of updates in a test is independent from previous
        tests.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        self._sl4f.run(method=_Sl4fMethods.SET_NEW_UPDATE_LISTENER)

    def start_client_connections(self) -> None:
        """Enables device to initiate connections to networks.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        self._sl4f.run(method=_Sl4fMethods.START_CLIENT_CONNECTIONS)

    def stop_client_connections(self) -> None:
        """Disables device for initiating connections to networks.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        self._sl4f.run(method=_Sl4fMethods.STOP_CLIENT_CONNECTIONS)
