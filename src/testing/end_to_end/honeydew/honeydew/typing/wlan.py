#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by wlan affordance."""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Protocol


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


class RequestStatus(enum.StrEnum):
    """Connect request response.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.common/wlan_common.fidl
    """

    ACKNOWLEDGED = "Acknowledged"
    REJECTED_NOT_SUPPORTED = "RejectedNotSupported"
    REJECTED_INCOMPATIBLE_MODE = "RejectedIncompatibleMode"
    REJECTED_ALREADY_IN_USE = "RejectedAlreadyInUse"
    REJECTED_DUPLICATE_REQUEST = "RejectedDuplicateRequest"


class WlanMacRole(enum.StrEnum):
    """Role of the WLAN MAC interface.

    Loosely matches the fuchsia.wlan.common.WlanMacRole FIDL enum.
    See https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    CLIENT = "Client"
    AP = "Ap"
    MESH = "Mesh"
    UNKNOWN = "Unknown"


class BssType(enum.StrEnum):
    """BssType

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    INFRASTRUCTURE = "Infrastructure"
    PERSONAL = "Personal"
    INDEPENDENT = "Independent"
    MESH = "Mesh"
    UNKNOWN = "Unknown"


class ChannelBandwidth(enum.StrEnum):
    """Channel Bandwidth

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    CBW20 = "Cbw20"
    CBW40 = "Cbw40"
    CBW40BELOW = "Cbw40Below"
    CBW80 = "Cbw80"
    CBW160 = "Cbw160"
    CBW80P80 = "Cbw80P80"
    UNKNOWN = "Unknown"


class Protection(enum.IntEnum):
    """Protection

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    UNKNOWN = 0
    OPEN = 1
    WEP = 2
    WPA1 = 3
    WPA1_WPA2_PERSONAL_TKIP_ONLY = 4
    WPA2_PERSONAL_TKIP_ONLY = 5
    WPA1_WPA2_PERSONAL = 6
    WPA2_PERSONAL = 7
    WPA2_WPA3_PERSONAL = 8
    WPA3_PERSONAL = 9
    WPA2_ENTERPRISE = 10
    WPA3_ENTERPRISE = 11


@dataclass(frozen=True)
class NetworkConfig:
    """Network information used to establish a connection.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """

    ssid: str
    security_type: SecurityType
    credential_type: str
    credential_value: str

    def __lt__(self, other: NetworkConfig) -> bool:
        return self.ssid < other.ssid


@dataclass(frozen=True)
class NetworkIdentifier:
    """Combination of ssid and the security type.

    Primary means of distinguishing between available networks.
    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """

    ssid: str
    security_type: SecurityType

    def __lt__(self, other: NetworkIdentifier) -> bool:
        return self.ssid < other.ssid


@dataclass(frozen=True)
class NetworkState:
    """Information about a network's current connections and attempts.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    network_identifier: NetworkIdentifier
    connection_state: ConnectionState
    disconnect_status: DisconnectStatus

    def __lt__(self, other: NetworkState) -> bool:
        return self.network_identifier < other.network_identifier


@dataclass(frozen=True)
class ClientStateSummary:
    """Information about the current client state for the device.

    This includes if the device will attempt to connect to access points
    (when applicable), any existing connections and active connection attempts
    and their outcomes.
    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    state: WlanClientState
    networks: list[NetworkState]

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, ClientStateSummary):
            return NotImplemented
        return self.state == other.state and sorted(self.networks) == sorted(
            other.networks
        )


@dataclass(frozen=True)
class WlanChannel:
    """Wlan channel information.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    primary: int
    cbw: ChannelBandwidth
    secondary80: int


@dataclass(frozen=True)
class QueryIfaceResponse:
    """Queryiface response

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    role: WlanMacRole
    id: int
    phy_id: int
    phy_assigned_id: int
    sta_addr: list[int]


@dataclass(frozen=True)
class BssDescription:
    """BssDescription

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    bssid: list[int]
    bss_type: BssType
    beacon_period: int
    capability_info: int
    ies: list[int]
    channel: WlanChannel
    rssi_dbm: int
    snr_db: int


@dataclass(frozen=True)
class ClientStatusResponse(Protocol):
    def status(self) -> str:
        ...


@dataclass(frozen=True)
class ClientStatusConnected(ClientStatusResponse):
    """ServingApInfo, returned as a part of ClientStatusResponse.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    bssid: list[int]
    ssid: list[int]
    rssi_dbm: int
    snr_db: int
    channel: WlanChannel
    protection: Protection

    def status(self) -> str:
        return "Connected"


@dataclass(frozen=True)
class ClientStatusConnecting(ClientStatusResponse):
    ssid: list[int]

    def status(self) -> str:
        return "Connecting"


@dataclass(frozen=True)
class ClientStatusIdle(ClientStatusResponse):
    def status(self) -> str:
        return "Idle"


class CountryCode(enum.StrEnum):
    """Country codes used for configuring WLAN.

    This is a list countries and their respective Alpha-2 codes. It comes from
    http://cs/h/turquoise-internal/turquoise/+/main:src/devices/board/drivers/nelson/nelson-sdio.cc?l=79.
    TODO(http://b/337930095): We need to add the other product specific country code
    lists and test for them.
    """

    AUSTRIA = "AT"
    AUSTRALIA = "AU"
    BELGIUM = "BE"
    BULGARIA = "BG"
    CANADA = "CA"
    SWITZERLAND = "CH"
    CHILE = "CL"
    COLOMBIA = "CO"
    CYPRUS = "CY"
    CZECHIA = "CZ"
    GERMANY = "DE"
    DENMARK = "DK"
    ESTONIA = "EE"
    GREECE_EU = "EL"
    SPAIN = "ES"
    FINLAND = "FI"
    FRANCE = "FR"
    UNITED_KINGDOM_OF_GREAT_BRITAIN = "GB"
    GREECE = "GR"
    CROATIA = "HR"
    HUNGARY = "HU"
    IRELAND = "IE"
    INDIA = "IN"
    ICELAND = "IS"
    ITALY = "IT"
    JAPAN = "JP"
    KOREA = "KR"
    LIECHTENSTEIN = "LI"
    LITHUANIA = "LT"
    LUXEMBOURG = "LU"
    LATVIA = "LV"
    MALTA = "MT"
    MEXICO = "MX"
    NETHERLANDS = "NL"
    NORWAY = "NO"
    NEW_ZEALAND = "NZ"
    PERU = "PE"
    POLAND = "PL"
    PORTUGAL = "PT"
    ROMANIA = "RO"
    SWEDEN = "SE"
    SINGAPORE = "SG"
    SLOVENIA = "SI"
    SLOVAKIA = "SK"
    TURKEY = "TR"
    TAIWAN = "TW"
    UNITED_STATES_OF_AMERICA = "US"
    WORLDWIDE = "WW"
