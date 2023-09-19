#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by wlan affordance."""

from __future__ import annotations

from dataclasses import dataclass
import enum


# pylint: disable=line-too-long
# TODO(b/299995309): Add lint if change presubmit checks to keep enums and fidl
# definitions consistent.
class SecurityType(enum.StrEnum):
    """Fuchsia supported security types.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """
    NONE = "none"
    WEP = "wep"
    WPA = "wpa"
    WPA2 = "wpa2"
    WPA3 = "wpa3"


class WlanClientState(enum.StrEnum):
    """Wlan operating state for client connections.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """
    CONNECTIONS_DISABLED = "ConnectionsDisabled"
    CONNECTIONS_ENABLED = "ConnectionsEnabled"


class ConnectionState(enum.StrEnum):
    """Connection states used to update registered wlan observers.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """
    FAILED = "Failed"
    DISCONNECTED = "Disconnected"
    CONNECTING = "Connecting"
    CONNECTED = "Connected"


class DisconnectStatus(enum.StrEnum):
    """Disconnect and connection attempt failure status codes.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """
    TIMED_OUT = "TimedOut"
    CREDENTIALS_FAILED = "CredentialsFailed"
    CONNECTION_STOPPED = "ConnectionStopped"
    CONNECTION_FAILED = "ConnectionFailed"


@dataclass
class NetworkConfig:
    """Network information used to establish a connection.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """
    ssid: str
    security_type: SecurityType
    credential_type: str
    credential_value: str

    def __lt__(self, other) -> bool:
        return self.ssid < other.ssid


@dataclass
class NetworkIdentifier:
    """Combination of ssid and the security type.

    Primary means of distinguishing between available networks.
    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """
    ssid: str
    security_type: SecurityType

    def __lt__(self, other) -> bool:
        return self.ssid < other.ssid


@dataclass
class NetworkState:
    """Information about a network's current connections and attempts.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """
    network_identifier: NetworkIdentifier
    connection_state: ConnectionState
    disconnect_status: DisconnectStatus

    def __lt__(self, other) -> bool:
        return self.network_identifier < other.network_identifier


@dataclass
class ClientStateSummary:
    """Information about the current client state for the device.

    This includes if the device will attempt to connect to access points
    (when applicable), any existing connections and active connection attempts
    and their outcomes.
    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """
    state: WlanClientState
    networks: list[NetworkState]

    def __eq__(self, other) -> bool:
        return self.state == other.state and sorted(self.networks) == sorted(
            other.networks)
