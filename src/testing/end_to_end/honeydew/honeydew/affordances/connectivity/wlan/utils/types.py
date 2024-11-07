# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Data types used by wlan affordance."""

from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Protocol

import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_common_security as f_wlan_common_security
import fidl.fuchsia_wlan_device_service as f_wlan_device_service
import fidl.fuchsia_wlan_policy as f_wlan_policy
import fidl.fuchsia_wlan_sme as f_wlan_sme

# Length of a pre-shared key (PSK) used as a password.
_PSK_LENGTH = 64


class Implementation(enum.StrEnum):
    """Different WLAN affordance implementations available."""

    # Use WLAN affordances that is implemented using Fuchsia-Controller
    FUCHSIA_CONTROLLER = "fuchsia-controller"

    # Use WLAN affordances that is implemented using SL4F
    SL4F = "sl4f"


@dataclass(frozen=True)
class MacAddress:
    """MAC address following the EUI-48 identifier format.

    Used by IEEE 802 networks as unique identifiers assigned to network
    interface controllers.
    """

    mac: str
    """MAC address in the form "xx:xx:xx:xx:xx:xx"."""

    @staticmethod
    def from_bytes(b: bytes) -> "MacAddress":
        """Create a MacAddress from bytes."""
        if len(b) != 6:
            raise ValueError(f"Expected 6 bytes, got {len(b)}")
        mac = ":".join([f"{octet:0>2x}" for octet in b])
        return MacAddress(mac)

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

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.SecurityType) -> "SecurityType":
        """Parse from a fuchsia.wlan.policy/SecurityType."""
        match int(fidl):
            case f_wlan_policy.SecurityType.NONE:
                return SecurityType.NONE
            case f_wlan_policy.SecurityType.WEP:
                return SecurityType.WEP
            case f_wlan_policy.SecurityType.WPA:
                return SecurityType.WPA
            case f_wlan_policy.SecurityType.WPA2:
                return SecurityType.WPA2
            case f_wlan_policy.SecurityType.WPA3:
                return SecurityType.WPA3
            case _:
                raise TypeError(f"Unknown SecurityType: {fidl}")

    def to_fidl(self) -> f_wlan_policy.SecurityType:
        """Convert to a fuchsia.wlan.policy/SecurityType."""
        match self:
            case SecurityType.NONE:
                return f_wlan_policy.SecurityType.NONE
            case SecurityType.WEP:
                return f_wlan_policy.SecurityType.WEP
            case SecurityType.WPA:
                return f_wlan_policy.SecurityType.WPA
            case SecurityType.WPA2:
                return f_wlan_policy.SecurityType.WPA2
            case SecurityType.WPA3:
                return f_wlan_policy.SecurityType.WPA3


class WlanClientState(enum.StrEnum):
    """Wlan operating state for client connections.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    CONNECTIONS_DISABLED = "ConnectionsDisabled"
    CONNECTIONS_ENABLED = "ConnectionsEnabled"

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.WlanClientState) -> "WlanClientState":
        """Parse from a fuchsia.wlan.policy/WlanClientState."""
        match int(fidl):
            case f_wlan_policy.WlanClientState.CONNECTIONS_DISABLED:
                return WlanClientState.CONNECTIONS_DISABLED
            case f_wlan_policy.WlanClientState.CONNECTIONS_ENABLED:
                return WlanClientState.CONNECTIONS_ENABLED
            case _:
                raise TypeError(f"Unknown WlanClientState: {fidl}")


class ConnectionState(enum.StrEnum):
    """Connection states used to update registered wlan observers.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    FAILED = "Failed"
    DISCONNECTED = "Disconnected"
    CONNECTING = "Connecting"
    CONNECTED = "Connected"

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.ConnectionState) -> "ConnectionState":
        """Parse from a fuchsia.wlan.policy/ConnectionState."""
        match int(fidl):
            case f_wlan_policy.ConnectionState.FAILED:
                return ConnectionState.FAILED
            case f_wlan_policy.ConnectionState.DISCONNECTED:
                return ConnectionState.DISCONNECTED
            case f_wlan_policy.ConnectionState.CONNECTING:
                return ConnectionState.CONNECTING
            case f_wlan_policy.ConnectionState.CONNECTED:
                return ConnectionState.CONNECTED
            case _:
                raise TypeError(f"Unknown ConnectionState: {fidl}")


class DisconnectStatus(enum.StrEnum):
    """Disconnect and connection attempt failure status codes.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    TIMED_OUT = "TimedOut"
    CREDENTIALS_FAILED = "CredentialsFailed"
    CONNECTION_STOPPED = "ConnectionStopped"
    CONNECTION_FAILED = "ConnectionFailed"

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.DisconnectStatus) -> "DisconnectStatus":
        """Parse from a fuchsia.wlan.policy/DisconnectStatus."""
        match int(fidl):
            case f_wlan_policy.DisconnectStatus.TIMED_OUT:
                return DisconnectStatus.TIMED_OUT
            case f_wlan_policy.DisconnectStatus.CREDENTIALS_FAILED:
                return DisconnectStatus.CREDENTIALS_FAILED
            case f_wlan_policy.DisconnectStatus.CONNECTION_STOPPED:
                return DisconnectStatus.CONNECTION_STOPPED
            case f_wlan_policy.DisconnectStatus.CONNECTION_FAILED:
                return DisconnectStatus.CONNECTION_FAILED
            case _:
                raise TypeError(f"Unknown DisconnectStatus: {fidl}")


class RequestStatus(enum.StrEnum):
    """Connect request response.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.common/wlan_common.fidl
    """

    ACKNOWLEDGED = "Acknowledged"
    REJECTED_NOT_SUPPORTED = "RejectedNotSupported"
    REJECTED_INCOMPATIBLE_MODE = "RejectedIncompatibleMode"
    REJECTED_ALREADY_IN_USE = "RejectedAlreadyInUse"
    REJECTED_DUPLICATE_REQUEST = "RejectedDuplicateRequest"

    @staticmethod
    def from_fidl(fidl: f_wlan_common.RequestStatus) -> "RequestStatus":
        """Parse from a fuchsia.wlan.common/RequestStatus."""
        match int(fidl):
            case f_wlan_common.RequestStatus.ACKNOWLEDGED:
                return RequestStatus.ACKNOWLEDGED
            case f_wlan_common.RequestStatus.REJECTED_NOT_SUPPORTED:
                return RequestStatus.REJECTED_NOT_SUPPORTED
            case f_wlan_common.RequestStatus.REJECTED_INCOMPATIBLE_MODE:
                return RequestStatus.REJECTED_INCOMPATIBLE_MODE
            case f_wlan_common.RequestStatus.REJECTED_ALREADY_IN_USE:
                return RequestStatus.REJECTED_ALREADY_IN_USE
            case f_wlan_common.RequestStatus.REJECTED_DUPLICATE_REQUEST:
                return RequestStatus.REJECTED_DUPLICATE_REQUEST
            case _:
                raise TypeError(f"Unknown DisconnectStatus: {fidl}")


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
        """Parse from a fuchsia.wlan.common/WlanMacRole."""
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
        """Convert to a fuchsia.wlan.common/WlanMacRole."""
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
        """Parse from a fuchsia.wlan.sme/Protection."""
        return Protection(fidl)


@dataclass(frozen=True)
class NetworkConfig:
    """Network information used to establish a connection.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/types.fidl
    """

    ssid: str
    security_type: SecurityType
    credential_type: str
    credential_value: str

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.NetworkConfig) -> "NetworkConfig":
        """Parse from a fuchsia.wlan.policy/NetworkConfig."""
        identifier = NetworkIdentifier.from_fidl(fidl.id)
        credential = Credential.from_fidl(fidl.credential)
        return NetworkConfig(
            ssid=identifier.ssid,
            security_type=identifier.security_type,
            credential_type=credential.type(),
            credential_value=credential.value(),
        )

    def to_fidl(self) -> f_wlan_policy.NetworkConfig:
        """Convert to equivalent FIDL."""
        return f_wlan_policy.NetworkConfig(
            id=NetworkIdentifier(self.ssid, self.security_type).to_fidl(),
            credential=Credential.from_password(
                self.credential_value
            ).to_fidl(),
        )

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

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.NetworkIdentifier) -> "NetworkIdentifier":
        """Parse from a fuchsia.wlan.policy/NetworkIdentifier."""

        return NetworkIdentifier(
            ssid=bytes(fidl.ssid).decode("utf-8"),
            security_type=SecurityType.from_fidl(fidl.type),
        )

    def to_fidl(self) -> f_wlan_policy.NetworkIdentifier:
        """Convert to a fuchsia.wlan.policy/NetworkIdentifier."""
        return f_wlan_policy.NetworkIdentifier(
            ssid=list(self.ssid.encode("utf-8")),
            type=self.security_type.to_fidl(),
        )

    def __lt__(self, other: NetworkIdentifier) -> bool:
        return self.ssid < other.ssid


class Credential(Protocol):
    """Information used to verify access to a target network."""

    def type(self) -> str:
        """Type of credential."""

    def value(self) -> str:
        """Value of the credential, or empty string if not applicable."""

    def to_fidl(self) -> f_wlan_policy.Credential:
        """Convert to a fuchsia.wlan.policy/Credential."""

    @staticmethod
    def from_password(password: str | None) -> Credential:
        """Parse a password into a Credential.

        Args:
            password: String password, pre-shared key in hex form with length 64, or
                None/empty to represent open.

        Return:
            A fuchsia.wlan.policy/Credential union object.
        """
        if not password:
            return CredentialNone()
        elif len(password) == _PSK_LENGTH:
            return CredentialPsk(password)
        else:
            return CredentialPassword(password)

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.Credential) -> Credential:
        """Parse a fuchsia.wlan.policy/Credential."""
        if fidl.none is not None:
            return CredentialNone()
        if fidl.password is not None:
            return CredentialPassword(bytes(fidl.password).decode("utf-8"))
        if fidl.psk is not None:
            return CredentialPsk(bytes(fidl.psk).hex())
        raise TypeError(
            f"Unknown value for fuchsia.wlan.policy/Credential: {fidl}"
        )


class CredentialNone(Credential):
    """Credentials to connect to an unprotected network."""

    def type(self) -> str:
        return "None"

    def value(self) -> str:
        return ""

    def to_fidl(self) -> f_wlan_policy.Credential:
        cred = f_wlan_policy.Credential()
        cred.none = f_wlan_policy.Empty()
        return cred


@dataclass(frozen=True)
class CredentialPassword(Credential):
    """Credentials to connect to an password protected network."""

    password: str
    """Plaintext password."""

    def type(self) -> str:
        return "Password"

    def value(self) -> str:
        return self.password

    def to_fidl(self) -> f_wlan_policy.Credential:
        cred = f_wlan_policy.Credential()
        cred.password = list(self.password.encode("utf-8"))
        return cred


@dataclass(frozen=True)
class CredentialPsk(Credential):
    """Credentials to connect to an network using a pre-shared key."""

    psk: str
    """Hash representation of the network passphrase."""

    def type(self) -> str:
        return "Psk"

    def value(self) -> str:
        return self.psk

    def to_fidl(self) -> f_wlan_policy.Credential:
        cred = f_wlan_policy.Credential()
        cred.psk = list(bytes.fromhex(self.psk))
        return cred


@dataclass(frozen=True)
class NetworkState:
    """Information about a network's current connections and attempts.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.wlan.policy/client_provider.fidl
    """

    network_identifier: NetworkIdentifier
    connection_state: ConnectionState
    disconnect_status: DisconnectStatus | None

    @staticmethod
    def from_fidl(fidl: f_wlan_policy.NetworkState) -> "NetworkState":
        """Parse from a fuchsia.wlan.policy/NetworkState."""
        return NetworkState(
            network_identifier=NetworkIdentifier.from_fidl(fidl.id),
            connection_state=ConnectionState.from_fidl(fidl.state),
            disconnect_status=(
                DisconnectStatus.from_fidl(fidl.status) if fidl.status else None
            ),
        )

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

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.ClientStateSummary,
    ) -> "ClientStateSummary":
        """Parse from a fuchsia.wlan.policy/ClientStateSummary."""
        return ClientStateSummary(
            state=WlanClientState.from_fidl(fidl.state),
            networks=[NetworkState.from_fidl(n) for n in fidl.networks],
        )

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
        """Parse from a fuchsia.wlan.common/WlanChannel."""
        return WlanChannel(
            primary=fidl.primary,
            cbw=ChannelBandwidth.from_fidl(fidl.cbw),
            secondary80=fidl.secondary80,
        )

    def to_fidl(self) -> f_wlan_common.WlanChannel:
        """Convert to a fuchsia.wlan.common/WlanChannel."""
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
        """Parse from a fuchsia.wlan.device.service/QueryIfaceResponse."""
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
    def from_fidl(fidl: f_wlan_common.BssDescription) -> "BssDescription":
        """Parse from a fuchsia.wlan.common.BssDescription."""
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

    def to_fidl(self) -> f_wlan_common.BssDescription:
        """Convert to a fuchsia.wlan.common.BssDescription."""
        return f_wlan_common.BssDescription(
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
# typing for FIDL. Once these static types are available, replace with the
# statically generated FIDL equivalent.
@dataclass(frozen=True)
class Authentication:
    """Pairs credentials with a particular security protocol.

    This type requires validation, as `protocol` and `credentials` may disagree.
    """

    protocol: SecurityProtocol
    """Security protocol."""

    credentials: Credentials | None
    """Credentials to pair with the security protocol."""

    def to_fidl(self) -> f_wlan_common_security.Authentication:
        """Convert to a fuchsia.wlan.common.security/Authentication."""
        return f_wlan_common_security.Authentication(
            protocol=self.protocol,
            credentials=(
                self.credentials.to_fidl() if self.credentials else None
            ),
        )


class SecurityProtocol(enum.IntEnum):
    """WLAN security protocols."""

    OPEN = 1
    """Open network security.

    This indicates that no security protocol or suite is used by a WLAN; it
    is not to be confused with "open authentication".
    """

    WEP = 2
    WPA1 = 3
    WPA2_PERSONAL = 4
    WPA2_ENTERPRISE = 5
    WPA3_PERSONAL = 6
    WPA3_ENTERPRISE = 7


class Credentials(Protocol):
    """Credentials used to authenticate with a WLAN."""

    def to_fidl(self) -> f_wlan_common_security.Credentials:
        """Convert to a fuchsia.wlan.common.security/Credentials."""


@dataclass(frozen=True)
class WepCredentials(Credentials):
    """WEP credentials."""

    key: str
    """Unencoded WEP key.

    This field is always a binary key; ASCII hexadecimal encoding should not be
    used here.
    """

    def to_fidl(self) -> f_wlan_common_security.Credentials:
        """Convert to a fuchsia.wlan.common.security/Credentials."""
        credentials = f_wlan_common_security.Credentials()
        credentials.wep = f_wlan_common_security.WepCredentials(
            key=list(self.key.encode("utf-8"))
        )
        return credentials


@dataclass(frozen=True)
class WpaPskCredentials(Credentials):
    """WPA-PSK credentials."""

    psk: bytes
    """
    Unencoded pre-shared key (PSK).

    This field is always a binary key; ASCII hexadecimal encoding should not be
    used here.
    """

    def to_fidl(self) -> f_wlan_common_security.Credentials:
        """Convert to a fuchsia.wlan.common.security/Credentials."""
        credentials = f_wlan_common_security.Credentials()
        credentials.wpa = f_wlan_common_security.WpaCredentials()
        credentials.wpa.psk = list(self.psk)
        return credentials


@dataclass(frozen=True)
class WpaPassphraseCredentials(Credentials):
    """WPA credentials with passphrase."""

    passphrase: str
    """
    UTF-8 encoded passphrase.

    This field is expected to use UTF-8 or compatible encoding. This is more
    permissive than the passphrase to PSK mapping specified in IEEE Std
    802.11-2016 Appendix J.4, but UTF-8 is typically used in practice.
    """

    def to_fidl(self) -> f_wlan_common_security.Credentials:
        """Convert to a fuchsia.wlan.common.security/Credentials."""
        credentials = f_wlan_common_security.Credentials()
        credentials.wpa = f_wlan_common_security.WpaCredentials()
        credentials.wpa.passphrase = list(self.passphrase.encode("utf-8"))
        return credentials


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
        """Parse from a fuchsia.wlan.sme/ClientStatusResponse."""
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


class ConnectivityMode(enum.IntEnum):
    """Connectivity operating mode for the access point."""

    LOCAL_ONLY = 1
    """Allows for connectivity between co-located devices.

    Local only access points do not forward traffic to other network connections.
    """

    UNRESTRICTED = 2
    """Allows for full connectivity.

    Traffic can potentially being forwarded to other network connections (e.g.
    tethering mode).
    """

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.ConnectivityMode,
    ) -> "ConnectivityMode":
        """Parse from a fuchsia.wlan.policy/ConnectivityMode."""
        return ConnectivityMode(fidl)

    def to_fidl(self) -> f_wlan_policy.ConnectivityMode:
        """Convert to equivalent FIDL."""
        return f_wlan_policy.ConnectivityMode(self.value)


class OperatingBand(enum.IntEnum):
    """Operating band for wlan control request and status updates."""

    ANY = 1
    """Allows for band switching depending on device operating mode and
    environment."""

    ONLY_2_4GHZ = 2
    """Restricted to 2.4 GHz bands only."""

    ONLY_5GHZ = 3
    """Restricted to 5 GHz bands only."""

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.OperatingBand,
    ) -> "OperatingBand":
        """Parse from a fuchsia.wlan.policy/OperatingBand."""
        return OperatingBand(fidl)

    def to_fidl(self) -> f_wlan_policy.OperatingBand:
        """Convert to equivalent FIDL."""
        return f_wlan_policy.OperatingBand(self.value)


class OperatingState(enum.IntEnum):
    """Current detailed operating state for an access point."""

    FAILED = 1
    """Access point operation failed.

    Access points that enter the failed state will have one update informing
    registered listeners of the failure and then an additional update with the
    access point removed from the list.
    """

    STARTING = 2
    """Access point operation is starting up."""

    ACTIVE = 3
    """Access point operation is active."""

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.OperatingState,
    ) -> "OperatingState":
        """Parse from a fuchsia.wlan.policy/OperatingState."""
        return OperatingState(fidl)


@dataclass(frozen=True)
class AccessPointState:
    """Information about the individual operating access points.

    This includes limited information about any connected clients.
    """

    state: OperatingState
    """Current access point operating state."""

    mode: ConnectivityMode
    """Requested operating connectivity mode."""

    band: OperatingBand
    """Access point operating band."""

    frequency: int | None
    """Access point operating frequency (in MHz)."""

    clients: ConnectedClientInformation | None
    """Information about connected clients."""

    id: NetworkIdentifier
    """Identifying information of the access point whose state has changed."""

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.AccessPointState,
    ) -> "AccessPointState":
        """Parse from a fuchsia.wlan.policy/AccessPointState."""
        return AccessPointState(
            state=OperatingState.from_fidl(fidl.state),
            mode=ConnectivityMode.from_fidl(fidl.mode),
            band=OperatingBand.from_fidl(fidl.band),
            frequency=fidl.frequency,
            clients=(
                ConnectedClientInformation.from_fidl(fidl.clients)
                if fidl.clients
                else None
            ),
            id=NetworkIdentifier.from_fidl(fidl.id),
        )


@dataclass(frozen=True)
class ConnectedClientInformation:
    """Connected client information.

    This is initially limited to the number of connected clients.
    """

    count: int
    """Number of connected clients."""

    @staticmethod
    def from_fidl(
        fidl: f_wlan_policy.ConnectedClientInformation,
    ) -> "ConnectedClientInformation":
        """Parse from a fuchsia.wlan.policy/ConnectedClientInformation."""
        return ConnectedClientInformation(count=fidl.count)
