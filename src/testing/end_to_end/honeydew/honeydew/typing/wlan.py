# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by wlan affordance."""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Protocol

import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_device_service as f_wlan_device_service
import fidl.fuchsia_wlan_internal as f_wlan_internal
import fidl.fuchsia_wlan_sme as f_wlan_sme


@dataclass(frozen=True)
class MacAddress:
    """MAC address following the EUI-48 identifier format.

    Used by IEEE 802 networks as unique identifiers assigned to network
    interface controllers.
    """

    mac: str
    """MAC address in the form "xx:xx:xx:xx:xx:xx"."""

    def __str__(self) -> str:
        """Return MAC address in the form "xx:xx:xx:xx:xx:xx"."""
        return self.mac

    def bytes(self) -> bytes:
        """Convert MAC into a byte array.

        Returns:
            Byte array of the MAC address.

        Raises:
            ValueError: Invalid MAC address
        """
        try:
            mac = bytes([int(a, 16) for a in self.mac.split(":", 5)])
            for i, octet in enumerate(mac):
                if octet > 255:
                    raise ValueError(
                        f"Invalid octet at index {i}: larger than 8 bits"
                    )
            if len(mac) != 6:
                raise ValueError(f"Expected 6 bytes, got {len(mac)}")
            return mac
        except ValueError as e:
            raise ValueError("Invalid MAC address") from e


# pylint: disable=line-too-long
# TODO(http://b/299995309): Add lint if change presubmit checks to keep enums and fidl
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


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
class WlanMacRole(enum.StrEnum):
    """Role of the WLAN MAC interface.

    Loosely matches the fuchsia.wlan.common.WlanMacRole FIDL enum.
    See https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    CLIENT = "Client"
    AP = "Ap"
    MESH = "Mesh"
    UNKNOWN = "Unknown"

    @staticmethod
    def from_fidl(fidl: f_wlan_common.WlanMacRole) -> "WlanMacRole":
        match int(fidl):
            case 1:
                return WlanMacRole.CLIENT
            case 2:
                return WlanMacRole.AP
            case 3:
                return WlanMacRole.MESH
            case _:
                raise TypeError(f"Unknown WlanMacRole: {fidl}")

    def to_fidl(self) -> f_wlan_common.WlanMacRole:
        match self:
            case WlanMacRole.CLIENT:
                return f_wlan_common.WlanMacRole.CLIENT
            case WlanMacRole.AP:
                return f_wlan_common.WlanMacRole.AP
            case WlanMacRole.MESH:
                return f_wlan_common.WlanMacRole.MESH
            case WlanMacRole.UNKNOWN:
                raise TypeError("No corresponding WlanMacRole for UNKNOWN")


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
class BssType(enum.StrEnum):
    """BssType

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    UNKNOWN = "Unknown"
    INFRASTRUCTURE = "Infrastructure"
    INDEPENDENT = "Independent"
    MESH = "Mesh"
    PERSONAL = "Personal"

    @staticmethod
    def from_fidl(fidl: f_wlan_common.BssType) -> "BssType":
        match fidl:
            case f_wlan_common.BssType.UNKNOWN:
                return BssType.UNKNOWN
            case f_wlan_common.BssType.INFRASTRUCTURE:
                return BssType.INFRASTRUCTURE
            case f_wlan_common.BssType.INDEPENDENT:
                return BssType.INDEPENDENT
            case f_wlan_common.BssType.MESH:
                return BssType.MESH
            case f_wlan_common.BssType.PERSONAL:
                return BssType.PERSONAL
            case _:
                raise TypeError(f"Unknown BssType FIDL value: {fidl}")

    def to_fidl(self) -> f_wlan_common.BssType:
        match self:
            case BssType.UNKNOWN:
                return f_wlan_common.BssType.UNKNOWN
            case BssType.INFRASTRUCTURE:
                return f_wlan_common.BssType.INFRASTRUCTURE
            case BssType.INDEPENDENT:
                return f_wlan_common.BssType.INDEPENDENT
            case BssType.MESH:
                return f_wlan_common.BssType.MESH
            case BssType.PERSONAL:
                return f_wlan_common.BssType.PERSONAL


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
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

    @staticmethod
    def from_fidl(fidl: f_wlan_common.ChannelBandwidth) -> "ChannelBandwidth":
        match fidl:
            case f_wlan_common.ChannelBandwidth.CBW20:
                return ChannelBandwidth.CBW20
            case f_wlan_common.ChannelBandwidth.CBW40:
                return ChannelBandwidth.CBW40
            case f_wlan_common.ChannelBandwidth.CBW40BELOW:
                return ChannelBandwidth.CBW40BELOW
            case f_wlan_common.ChannelBandwidth.CBW80:
                return ChannelBandwidth.CBW80
            case f_wlan_common.ChannelBandwidth.CBW80P80:
                return ChannelBandwidth.CBW80P80
            case _:
                raise TypeError(f"Unknown ChannelBandwidth FIDL value: {fidl}")

    def to_fidl(self) -> f_wlan_common.ChannelBandwidth:
        match self:
            case ChannelBandwidth.CBW20:
                return f_wlan_common.ChannelBandwidth.CBW20
            case ChannelBandwidth.CBW40:
                return f_wlan_common.ChannelBandwidth.CBW40
            case ChannelBandwidth.CBW40BELOW:
                return f_wlan_common.ChannelBandwidth.CBW40BELOW
            case ChannelBandwidth.CBW80:
                return f_wlan_common.ChannelBandwidth.CBW80
            case ChannelBandwidth.CBW80P80:
                return f_wlan_common.ChannelBandwidth.CBW80P80
            case ChannelBandwidth.UNKNOWN:
                raise TypeError(
                    "ChannelBandwidth.UNKNOWN doesn't have FIDL equivalent"
                )


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
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

    @staticmethod
    def from_fidl(fidl: f_wlan_sme.Protection) -> "Protection":
        match fidl:
            case p if isinstance(p, f_wlan_sme.Protection) and (
                int(p) >= 0 and int(p) <= 11
            ):
                return Protection(int(p))
            case _:
                raise TypeError(f"Unknown Protection FIDL value: {fidl}")


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

    @staticmethod
    def from_fidl(fidl: f_wlan_common.WlanChannel) -> "WlanChannel":
        return WlanChannel(
            primary=fidl.primary,
            cbw=ChannelBandwidth.from_fidl(fidl.cbw),
            secondary80=fidl.secondary80,
        )

    def to_fidl(self) -> f_wlan_common.WlanChannel:
        return WlanChannel(
            primary=self.primary,
            cbw=self.cbw.to_fidl(),
            secondary80=self.secondary80,
        )


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

    @staticmethod
    def from_fidl(
        fidl: f_wlan_device_service.QueryIfaceResponse,
    ) -> "QueryIfaceResponse":
        """Create a QueryIFaceResponse from the FIDL equivalent."""
        return QueryIfaceResponse(
            role=WlanMacRole.from_fidl(fidl.role),
            id=fidl.id,
            phy_id=fidl.phy_id,
            phy_assigned_id=fidl.phy_assigned_id,
            sta_addr=list(fidl.sta_addr),
        )


class InformationElementType(enum.IntEnum):
    """Information Element type.

    As defined by IEEE 802.11-1997 Section 7.3.2 and further expanded by
    802.11d, 802.11g, 802.11h, and 802.11i.

    https://www.oreilly.com/library/view/80211-wireless-networks/0596100523/ch04.html#wireless802dot112-CHP-4-TABLE-7
    """

    SSID = 0
    # Types 1-255 are not implemented. Only implement a new type if it is being used.


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
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

    @staticmethod
    def from_fidl(fidl: f_wlan_internal.BssDescription) -> "BssDescription":
        return BssDescription(
            bssid=list(fidl.bssid),
            bss_type=BssType.from_fidl(fidl.bss_type),
            beacon_period=fidl.beacon_period,
            capability_info=fidl.capability_info,
            ies=list(fidl.ies),
            channel=WlanChannel.from_fidl(fidl.channel),
            rssi_dbm=fidl.rssi_dbm,
            snr_db=fidl.snr_db,
        )

    def to_fidl(self) -> f_wlan_internal.BssDescription:
        return f_wlan_internal.BssDescription(
            bssid=self.bssid,
            bss_type=self.bss_type.to_fidl(),
            beacon_period=self.beacon_period,
            capability_info=self.capability_info,
            ies=self.ies,
            channel=self.channel.to_fidl(),
            rssi_dbm=self.rssi_dbm,
            snr_db=self.snr_db,
        )

    def ssid(self) -> str | None:
        """Parse information elements for SSID."""
        ies = bytes(self.ies)
        i = 0
        while i < len(ies):
            if not len(ies) > i + 1:
                raise TypeError(
                    "Invalid information element; requires at least 2 bytes for "
                    f"Element ID and Length, got {len(ies)-i}"
                )

            element = int(ies[i])
            length = int(ies[i + 1])
            i += 2

            try:
                ie_type = InformationElementType(int(element))
            except ValueError:
                # Type not implemented. It's okay to skip
                i += length
                continue

            match ie_type:
                case InformationElementType.SSID:
                    try:
                        return ies[i : i + length].decode("utf-8")
                    except UnicodeDecodeError:
                        # ssid is not valid UTF-8; fallback to counting bytes
                        return f"<ssid-{length}>"
                case _:
                    raise TypeError(
                        f"Unsupported InformationElementType: {ie_type}"
                    )

        return None


# TODO(http://b/346424966): Only necessary because Python does not have static
# typing for FIDL. Once these static types are available and the SL4F affordance
# is removed, replace with the statically generated FIDL equivalent.
@dataclass(frozen=True)
class ClientStatusResponse(Protocol):
    """WLAN client interface status."""

    def status(self) -> str:
        """Description of the client's status."""

    @staticmethod
    def from_fidl(
        fidl: f_wlan_sme.ClientStatusResponse,
    ) -> "ClientStatusResponse":
        """Convert a ClientStatusResponse FIDL to the corresponding type."""
        if fidl.connected:
            ap: f_wlan_sme.ServingApInfo = fidl.connected
            return ClientStatusConnected(
                bssid=list(ap.bssid),
                ssid=list(ap.ssid),
                rssi_dbm=ap.rssi_dbm,
                snr_db=ap.snr_db,
                channel=WlanChannel.from_fidl(ap.channel),
                protection=Protection.from_fidl(ap.protection),
            )

        if fidl.connecting:
            return ClientStatusConnecting(ssid=fidl.connecting)

        if fidl.idle:
            return ClientStatusIdle()

        raise TypeError(f"Unknown ClientStatusResponse FIDL value: {fidl}")


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
