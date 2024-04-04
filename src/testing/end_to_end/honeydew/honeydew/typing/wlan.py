#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Data types used by wlan affordance."""

from __future__ import annotations

import enum
from dataclasses import dataclass


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


class BssType(enum.IntEnum):
    """BssType

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    INFRASTRUCTURE = 1
    PERSONAL = 2
    INDEPENDENT = 3
    MESH = 4


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
    WPA1WPA2PERSONALTKIPONLY = 4
    WPA2PERSONALTKIPONLY = 5
    WPA1WPA2PERSONAL = 6
    WPA2PERSONAL = 7
    WPA2WPA3PERSONAL = 8
    WPA3PERSONAL = 9
    WPA2ENTERPRISE = 0
    WPA3ENTERPRISE = 1


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
class ServingApInfo:
    """ServingApInfo, returned as a part of ClientStatusResponse.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    bssid: list[int]
    ssid: list[int]
    rssi_dbm: int
    snr_db: int
    channel: WlanChannel
    protection: Protection


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
class ClientStatusResponse:
    """ClientStatusResponse returned from a status request.

    Defined by https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/sl4f/src/wlan/types.rs
    """

    connected: ServingApInfo
    connecting: list[int]
    idle: str


class CountryCodeList(enum.StrEnum):
    """Country codes used for configuring WLAN.

    This includes a list of countries and their respective Alpha-2 codes.

    Defined by https://en.wikipedia.org/wiki/ISO_3166-1
    """

    AFGHANISTAN = "AF"
    ALAND_ISLANDS = "AX"
    ALBANIA = "AL"
    ALGERIA = "DZ"
    AMERICAN_SAMOA = "AS"
    ANDORRA = "AD"
    ANGOLA = "AO"
    ANGUILLA = "AI"
    ANTARCTICA = "AQ"
    ANTIGUA_AND_BARBUDA = "AG"
    ARGENTINA = "AR"
    ARMENIA = "AM"
    ARUBA = "AW"
    AUSTRALIA = "AU"
    AUSTRIA = "AT"
    AZERBAIJAN = "AZ"
    BAHAMAS = "BS"
    BAHRAIN = "BH"
    BANGLADESH = "BD"
    BARBADOS = "BB"
    BELARUS = "BY"
    BELGIUM = "BE"
    BELIZE = "BZ"
    BENIN = "BJ"
    BERMUDA = "BM"
    BHUTAN = "BT"
    BOLIVIA = "BO"
    BONAIRE = "BQ"
    BOSNIA_AND_HERZEGOVINA = "BA"
    BOTSWANA = "BW"
    BOUVET_ISLAND = "BV"
    BRAZIL = "BR"
    BRITISH_INDIAN_OCEAN_TERRITORY = "IO"
    BRUNEI_DARUSSALAM = "BN"
    BULGARIA = "BG"
    BURKINA_FASO = "BF"
    BURUNDI = "BI"
    CAMBODIA = "KH"
    CAMEROON = "CM"
    CANADA = "CA"
    CAPE_VERDE = "CV"
    CAYMAN_ISLANDS = "KY"
    CENTRAL_AFRICAN_REPUBLIC = "CF"
    CHAD = "TD"
    CHILE = "CL"
    CHINA = "CN"
    CHRISTMAS_ISLAND = "CX"
    COCOS_ISLANDS = "CC"
    COLOMBIA = "CO"
    COMOROS = "KM"
    CONGO = "CG"
    DEMOCRATIC_REPUBLIC_CONGO = "CD"
    COOK_ISLANDS = "CK"
    COSTA_RICA = "CR"
    COTE_D_IVOIRE = "CI"
    CROATIA = "HR"
    CUBA = "CU"
    CURACAO = "CW"
    CYPRUS = "CY"
    CZECH_REPUBLIC = "CZ"
    DENMARK = "DK"
    DJIBOUTI = "DJ"
    DOMINICA = "DM"
    DOMINICAN_REPUBLIC = "DO"
    ECUADOR = "EC"
    EGYPT = "EG"
    EL_SALVADOR = "SV"
    EQUATORIAL_GUINEA = "GQ"
    ERITREA = "ER"
    ESTONIA = "EE"
    ETHIOPIA = "ET"
    FALKLAND_ISLANDS_MALVINAS = "FK"
    FAROE_ISLANDS = "FO"
    FIJI = "FJ"
    FINLAND = "FI"
    FRANCE = "FR"
    FRENCH_GUIANA = "GF"
    FRENCH_POLYNESIA = "PF"
    FRENCH_SOUTHERN_TERRITORIES = "TF"
    GABON = "GA"
    GAMBIA = "GM"
    GEORGIA = "GE"
    GERMANY = "DE"
    GHANA = "GH"
    GIBRALTAR = "GI"
    GREECE = "GR"
    GREENLAND = "GL"
    GRENADA = "GD"
    GUADELOUPE = "GP"
    GUAM = "GU"
    GUATEMALA = "GT"
    GUERNSEY = "GG"
    GUINEA = "GN"
    GUINEA_BISSAU = "GW"
    GUYANA = "GY"
    HAITI = "HT"
    HEARD_ISLAND_AND_MCDONALD_ISLANDS = "HM"
    VATICAN_CITY_STATE = "VA"
    HONDURAS = "HN"
    HONG_KONG = "HK"
    HUNGARY = "HU"
    ICELAND = "IS"
    INDIA = "IN"
    INDONESIA = "ID"
    IRAN = "IR"
    IRAQ = "IQ"
    IRELAND = "IE"
    ISLE_OF_MAN = "IM"
    ISRAEL = "IL"
    ITALY = "IT"
    JAMAICA = "JM"
    JAPAN = "JP"
    JERSEY = "JE"
    JORDAN = "JO"
    KAZAKHSTAN = "KZ"
    KENYA = "KE"
    KIRIBATI = "KI"
    DEMOCRATIC_PEOPLE_S_REPUBLIC_OF_KOREA = "KP"
    REPUBLIC_OF_KOREA = "KR"
    KUWAIT = "KW"
    KYRGYZSTAN = "KG"
    LAO = "LA"
    LATVIA = "LV"
    LEBANON = "LB"
    LESOTHO = "LS"
    LIBERIA = "LR"
    LIBYA = "LY"
    LIECHTENSTEIN = "LI"
    LITHUANIA = "LT"
    LUXEMBOURG = "LU"
    MACAO = "MO"
    MACEDONIA = "MK"
    MADAGASCAR = "MG"
    MALAWI = "MW"
    MALAYSIA = "MY"
    MALDIVES = "MV"
    MALI = "ML"
    MALTA = "MT"
    MARSHALL_ISLANDS = "MH"
    MARTINIQUE = "MQ"
    MAURITANIA = "MR"
    MAURITIUS = "MU"
    MAYOTTE = "YT"
    MEXICO = "MX"
    MICRONESIA = "FM"
    MOLDOVA = "MD"
    MONACO = "MC"
    MONGOLIA = "MN"
    MONTENEGRO = "ME"
    MONTSERRAT = "MS"
    MOROCCO = "MA"
    MOZAMBIQUE = "MZ"
    MYANMAR = "MM"
    NAMIBIA = "NA"
    NAURU = "NR"
    NEPAL = "NP"
    NETHERLANDS = "NL"
    NEW_CALEDONIA = "NC"
    NEW_ZEALAND = "NZ"
    NICARAGUA = "NI"
    NIGER = "NE"
    NIGERIA = "NG"
    NIUE = "NU"
    NORFOLK_ISLAND = "NF"
    NORTHERN_MARIANA_ISLANDS = "MP"
    NORWAY = "NO"
    OMAN = "OM"
    PAKISTAN = "PK"
    PALAU = "PW"
    PALESTINE = "PS"
    PANAMA = "PA"
    PAPUA_NEW_GUINEA = "PG"
    PARAGUAY = "PY"
    PERU = "PE"
    PHILIPPINES = "PH"
    PITCAIRN = "PN"
    POLAND = "PL"
    PORTUGAL = "PT"
    PUERTO_RICO = "PR"
    QATAR = "QA"
    REUNION = "RE"
    ROMANIA = "RO"
    RUSSIAN_FEDERATION = "RU"
    RWANDA = "RW"
    SAINT_BARTHELEMY = "BL"
    SAINT_KITTS_AND_NEVIS = "KN"
    SAINT_LUCIA = "LC"
    SAINT_MARTIN = "MF"
    SAINT_PIERRE_AND_MIQUELON = "PM"
    SAINT_VINCENT_AND_THE_GRENADINES = "VC"
    SAMOA = "WS"
    SAN_MARINO = "SM"
    SAO_TOME_AND_PRINCIPE = "ST"
    SAUDI_ARABIA = "SA"
    SENEGAL = "SN"
    SERBIA = "RS"
    SEYCHELLES = "SC"
    SIERRA_LEONE = "SL"
    SINGAPORE = "SG"
    SINT_MAARTEN = "SX"
    SLOVAKIA = "SK"
    SLOVENIA = "SI"
    SOLOMON_ISLANDS = "SB"
    SOMALIA = "SO"
    SOUTH_AFRICA = "ZA"
    SOUTH_GEORGIA = "GS"
    SOUTH_SUDAN = "SS"
    SPAIN = "ES"
    SRI_LANKA = "LK"
    SUDAN = "SD"
    SURINAME = "SR"
    SVALBARD_AND_JAN_MAYEN = "SJ"
    SWAZILAND = "SZ"
    SWEDEN = "SE"
    SWITZERLAND = "CH"
    SYRIAN_ARAB_REPUBLIC = "SY"
    TAIWAN = "TW"
    TAJIKISTAN = "TJ"
    TANZANIA = "TZ"
    THAILAND = "TH"
    TIMOR_LESTE = "TL"
    TOGO = "TG"
    TOKELAU = "TK"
    TONGA = "TO"
    TRINIDAD_AND_TOBAGO = "TT"
    TUNISIA = "TN"
    TURKEY = "TR"
    TURKMENISTAN = "TM"
    TURKS_AND_CAICOS_ISLANDS = "TC"
    TUVALU = "TV"
    UGANDA = "UG"
    UKRAINE = "UA"
    UNITED_ARAB_EMIRATES = "AE"
    UNITED_KINGDOM = "GB"
    UNITED_STATES = "US"
    UNITED_STATES_MINOR_OUTLYING_ISLANDS = "UM"
    URUGUAY = "UY"
    UZBEKISTAN = "UZ"
    VANUATU = "VU"
    VENEZUELA = "VE"
    VIETNAM = "VN"
    VIRGIN_ISLANDS_BRITISH = "VG"
    VIRGIN_ISLANDS_US = "VI"
    WALLIS_AND_FUTUNA = "WF"
    WESTERN_SAHARA = "EH"
    YEMEN = "YE"
    ZAMBIA = "ZM"
    ZIMBABWE = "ZW"
    NON_COUNTRY = "XX"
