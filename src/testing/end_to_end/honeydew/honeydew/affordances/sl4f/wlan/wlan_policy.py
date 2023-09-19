#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan policy affordance implementation using SL4F."""

from enum import StrEnum
import logging
from typing import Mapping

from honeydew.interfaces.affordances.wlan import wlan_policy
from honeydew.transports.sl4f import SL4F
from honeydew.typing.wlan import ClientStateSummary
from honeydew.typing.wlan import ConnectionState
from honeydew.typing.wlan import DisconnectStatus
from honeydew.typing.wlan import NetworkConfig
from honeydew.typing.wlan import NetworkIdentifier
from honeydew.typing.wlan import NetworkState
from honeydew.typing.wlan import SecurityType
from honeydew.typing.wlan import WlanClientState

_LOGGER: logging.Logger = logging.getLogger(__name__)


def _get_str(m: Mapping[str, object], key: str) -> str:
    val = m[key]
    if not isinstance(val, str):
        raise TypeError(f'Expected "{val}" to be str, got {type(val)}')
    return val


class Sl4fMethods(StrEnum):
    CREATE_CLIENT_CONTROLLER = "wlan_policy.create_client_controller"
    GET_SAVED_NETWORKS = "wlan_policy.get_saved_networks"
    GET_UPDATE = "wlan_policy.get_update"
    REMOVE_ALL_NETWORKS = "wlan_policy.remove_all_networks"
    SAVE_NETWORK = "wlan_policy.save_network"
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

    # List all the public methods in alphabetical order
    def create_client_controller(self) -> None:
        self._sl4f.run(method=Sl4fMethods.CREATE_CLIENT_CONTROLLER)

    def get_saved_networks(self) -> list[NetworkConfig]:
        resp: dict[str, object] = self._sl4f.run(
            method=Sl4fMethods.GET_SAVED_NETWORKS)
        result: object = resp.get("result", [])

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
                    credential_value=_get_str(n, "credential_value")))

        return networks

    def get_update(self) -> ClientStateSummary:
        """Gets one client listener update.

        This call will return with an update immediately the
        first time the update listener is initialized by setting a new listener
        or by creating a client controller before setting a new listener.
        Subsequent calls will hang until there is an update.

        Returns:
            An update of connection status. If there is no error, the result is
            a WlanPolicyUpdate with a structure that matches the FIDL
            ClientStateSummary struct given for updates.
        """
        resp: dict[str, object] = self._sl4f.run(method=Sl4fMethods.GET_UPDATE)
        result: object = resp.get("result", {})

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        if not isinstance(result["networks"], list):
            raise TypeError(
                'Expected "networks" to be list, '
                f'got {type(result["networks"])}')

        network_states: list[NetworkState] = []
        for n in result["networks"]:
            state: str | None = n["state"]
            status: str | None = n["status"]
            if state is None:
                state = ConnectionState.DISCONNECTED
            if status is None:
                status = DisconnectStatus.CONNECTION_STOPPED

            ssid: str = n["id"]["ssid"]
            security_type: str = n["id"]["type_"]

            network_states.append(
                NetworkState(
                    network_identifier=NetworkIdentifier(
                        ssid=ssid,
                        security_type=SecurityType(security_type.lower())),
                    connection_state=ConnectionState(state),
                    disconnect_status=DisconnectStatus(status)))

        return ClientStateSummary(
            state=WlanClientState(result["state"]), networks=network_states)

    def remove_all_networks(self) -> None:
        self._sl4f.run(method=Sl4fMethods.REMOVE_ALL_NETWORKS)

    def save_network(
            self,
            target_ssid: str,
            security_type: SecurityType,
            target_pwd: str | None = None) -> None:
        if not target_pwd:
            target_pwd = ""

        method_params: dict[str, object] = {
            "target_ssid": target_ssid,
            "security_type": str(security_type.value),
            "target_pwd": target_pwd,
        }
        self._sl4f.run(method=Sl4fMethods.SAVE_NETWORK, params=method_params)

    def set_new_update_listener(self) -> None:
        self._sl4f.run(method=Sl4fMethods.SET_NEW_UPDATE_LISTENER)

    def start_client_connections(self) -> None:
        self._sl4f.run(method=Sl4fMethods.START_CLIENT_CONNECTIONS)

    def stop_client_connections(self) -> None:
        self._sl4f.run(method=Sl4fMethods.STOP_CLIENT_CONNECTIONS)
