# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan affordance implementation using SL4F."""
import enum
from collections.abc import Mapping
from dataclasses import asdict

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    Authentication,
    BssDescription,
    BssType,
    ChannelBandwidth,
    ClientStatusConnected,
    ClientStatusConnecting,
    ClientStatusIdle,
    ClientStatusResponse,
    CountryCode,
    Protection,
    QueryIfaceResponse,
    WlanChannel,
    WlanMacRole,
)
from honeydew.affordances.connectivity.wlan.wlan import wlan
from honeydew.transports.sl4f import SL4F

STATUS_IDLE_KEY = "Idle"
STATUS_CONNECTING_KEY = "Connecting"

# We need to convert the string we receive from the wlan facade to an intEnum
# because serde gives us a string.
string_to_int_enum_map: dict[str, int] = {
    "Unknown": 0,
    "Open": 1,
    "Wep": 2,
    "Wpa1": 3,
    "Wpa1Wpa2PersonalTkipOnly": 4,
    "Wpa2PersonalTkipOnly": 5,
    "Wpa1Wpa2Personal": 6,
    "Wpa2Personal": 7,
    "Wpa2Wpa3Personal": 8,
    "Wpa3Personal": 9,
    "Wpa2Enterprise": 10,
    "Wpa3Enterprise": 11,
}


def _get_int(m: Mapping[str, object], key: str) -> int:
    val = m.get(key)
    if not isinstance(val, int):
        raise TypeError(f'Expected "{val}" to be int, got {type(val)}')
    return val


class _Sl4fMethods(enum.StrEnum):
    """Sl4f server commands."""

    CONNECT = "wlan.connect"
    CREATE_IFACE = "wlan.create_iface"
    DESTROY_IFACE = "wlan.destroy_iface"
    DISCONNECT = "wlan.disconnect"
    GET_COUNTRY = "wlan_phy.get_country"
    GET_IFACE_ID_LIST = "wlan.get_iface_id_list"
    GET_PHY_ID_LIST = "wlan.get_phy_id_list"
    QUERY_IFACE = "wlan.query_iface"
    SCAN_FOR_BSS_INFO = "wlan.scan_for_bss_info"
    SET_REGION = "location_regulatory_region_facade.set_region"
    STATUS = "wlan.status"


class Wlan(wlan.Wlan):
    """Wlan affordance implementation using SL4F.

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
        """Verifies that the WLAN affordance using SL4F is supported by the Fuchsia device.

        This method should be called in `__init__()` so that if this affordance was called on a
        Fuchsia device that does not support it, it will raise NotSupportedError.

        Raises:
            NotSupportedError: If affordance is not supported.
        """
        # TODO(http://b/377585939): To Be Implemented
        return

    def connect(
        self,
        ssid: str,
        # TODO(http://b/356234331): Remove the password field once
        # authentication is used everywhere.
        password: str | None,
        bss_desc: BssDescription,
        authentication: Authentication | None = None,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network. Deprecated; use
                authentication instead.
            bss_desc: The basic service set for target network.
            authentication: Authentication to connect with.

        Returns:
            True on success otherwise false.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: Return value not a bool.
        """
        method_params = {
            "target_ssid": ssid,
            "target_pwd": password,
            "target_bss_desc": asdict(bss_desc),
        }
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.CONNECT, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to connect") from e

        error = str(resp.get("error"))
        if "No wlan interface found" in error:
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No WLAN interface found"
            )
        elif "interface with role client not found" in error.lower():
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No client WLAN interface found"
            )

        result = resp.get("result")
        if not isinstance(result, bool):
            raise TypeError(f'Expected "result" to be bool, got {type(result)}')

        return result

    def create_iface(
        self, phy_id: int, role: WlanMacRole, sta_addr: str | None = None
    ) -> int:
        """Create a new WLAN interface.

        Args:
            phy_id: The iface ID.
            role: The role of the new iface.
            sta_addr: MAC address for softAP iface.

        Returns:
            Iface id of newly created interface.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
            TypeError: Return value not an int.
        """
        method_params = {
            "phy_id": phy_id,
            "role": role,
            "sta_addr": sta_addr,
        }
        try:
            resp = self._sl4f.run(
                method=_Sl4fMethods.CREATE_IFACE, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to create_iface") from e

        return _get_int(resp, "result")

    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
        """
        method_params = {"identifier": iface_id}
        try:
            self._sl4f.run(
                method=_Sl4fMethods.DESTROY_IFACE, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to destroy_iface"
            ) from e

    def disconnect(self) -> None:
        """Disconnect any current wifi connections.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
        """
        try:
            self._sl4f.run(method=_Sl4fMethods.DISCONNECT)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to disconnect") from e

    def get_country(self, phy_id: int) -> CountryCode:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
            TypeError: Return value not a list.
        """
        method_params = {"phy_id": phy_id}
        try:
            resp = self._sl4f.run(
                method=_Sl4fMethods.GET_COUNTRY, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to get_country") from e
        result = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        set_code = "".join([chr(ascii_char) for ascii_char in result])

        return CountryCode(set_code)

    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            HoneydewWlanError: On failure.
            TypeError: Return value not a list.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.GET_IFACE_ID_LIST
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to get_iface_id_list"
            ) from e
        result: object = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        return result

    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            HoneydewWlanError: On failure.
            TypeError: Return value not a list.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.GET_PHY_ID_LIST
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to get_phy_id_list"
            ) from e
        result: object = resp.get("result")

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        return result

    def query_iface(self, iface_id: int) -> QueryIfaceResponse:
        """Retrieves interface info for given wlan iface id.

        Args:
            iface_id: The wlan interface id to get info from.

        Returns:
            QueryIfaceResponseWrapper from the SL4F server.

        Raises:
            HoneydewWlanError: On failure.
            TypeError: If any of the return values are not of the expected type.
        """
        method_params = {"iface_id": iface_id}
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.QUERY_IFACE, params=method_params
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to query_iface") from e
        result: object = resp.get("result")

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        sta_addr = result.get("sta_addr")
        if not isinstance(sta_addr, list):
            raise TypeError(
                'Expected "sta_addr" to be list, ' f"got {type(sta_addr)}"
            )

        return QueryIfaceResponse(
            role=WlanMacRole(result.get("role", None)),
            id=_get_int(result, "id"),
            phy_id=_get_int(result, "phy_id"),
            phy_assigned_id=_get_int(result, "phy_assigned_id"),
            sta_addr=sta_addr,
        )

    def scan_for_bss_info(self) -> dict[str, list[BssDescription]]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            HoneydewWlanError: Sl4f run command failed.
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(
                method=_Sl4fMethods.SCAN_FOR_BSS_INFO
            )
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError(
                "Failed to scan_for_bss_info"
            ) from e

        error = str(resp.get("error"))
        if "No wlan interface found" in error:
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No WLAN interface found"
            )
        elif "interface with role client not found" in error.lower():
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No client WLAN interface found"
            )

        result: object = resp.get("result")
        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        ssid_bss_desc_map: dict[str, list[BssDescription]] = {}
        for ssid_key, bss_list in result.items():
            if not isinstance(bss_list, list):
                raise TypeError(
                    f'Expected "bss_list" to be list, got {type(bss_list)}'
                )

            # Create BssDescription type out of return values
            bss_descriptions: list[BssDescription] = []
            for bss in bss_list:
                bssid = bss.get("bssid")
                if not isinstance(bssid, list):
                    raise TypeError(
                        f'Expected "bssid" to be list, got {type(bssid)}'
                    )

                ies = bss.get("ies")
                if not isinstance(ies, list):
                    raise TypeError(
                        f'Expected "ies" to be list, got {type(ies)}'
                    )

                channel = bss.get("channel")
                if not isinstance(channel, dict):
                    raise TypeError(
                        f'Expected "channel" to be dict, got {type(channel)}'
                    )

                wlan_channel = WlanChannel(
                    primary=_get_int(channel, "primary"),
                    cbw=ChannelBandwidth(channel.get("cbw", None)),
                    secondary80=_get_int(channel, "secondary80"),
                )

                bss_block = BssDescription(
                    bssid=bssid,
                    bss_type=BssType(bss.get("bss_type", None)),
                    beacon_period=_get_int(bss, "beacon_period"),
                    capability_info=_get_int(bss, "capability_info"),
                    ies=ies,
                    channel=wlan_channel,
                    rssi_dbm=_get_int(bss, "rssi_dbm"),
                    snr_db=_get_int(bss, "snr_db"),
                )
                bss_descriptions.append(bss_block)

            ssid_bss_desc_map[ssid_key] = bss_descriptions

        return ssid_bss_desc_map

    def set_region(self, region_code: CountryCode) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            HoneydewWlanError: Sl4f run command failed.
        """
        method_params = {"region": region_code.value}
        try:
            self._sl4f.run(method=_Sl4fMethods.SET_REGION, params=method_params)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to set_region") from e

    def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            ClientStatusResponse which can be any one of three  things:
            ClientStatusConnected, ClientStatusConnecting, ClientStatusIdle.

        Raises:
            HoneydewWlanError: On failure.
            NetworkInterfaceNotFoundError: No client WLAN interface found.
            TypeError: If any of the return values are not of the expected type.
            ValueError: If none of the possible results are present.
        """
        try:
            resp: dict[str, object] = self._sl4f.run(method=_Sl4fMethods.STATUS)
        except errors.Sl4fError as e:
            raise wlan_errors.HoneydewWlanError("Failed to get status") from e

        error = str(resp.get("error"))
        if "No wlan interface found" in error:
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No WLAN interface found"
            )
        elif "interface with role client not found" in error.lower():
            raise wlan_errors.NetworkInterfaceNotFoundError(
                "No client WLAN interface found"
            )

        result: object = resp.get("result")
        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        # Only one of these keys in result should be present.
        if STATUS_IDLE_KEY in result:
            return ClientStatusIdle()
        elif STATUS_CONNECTING_KEY in result:
            ssid = result.get("Connecting")
            if not isinstance(ssid, list):
                raise TypeError(
                    f'Expected "connecting" to be list, got "{type(ssid)}"'
                )
            return ClientStatusConnecting(ssid=ssid)
        else:
            connected = result.get("Connected")
            if not isinstance(connected, dict):
                raise TypeError(
                    f'Expected "connected" to be dict, got {type(connected)}'
                )

            channel = connected.get("channel")
            if not isinstance(channel, dict):
                raise TypeError(
                    f'Expected "channel" to be dict, got {type(channel)}'
                )

            wlan_channel = WlanChannel(
                primary=_get_int(channel, "primary"),
                cbw=ChannelBandwidth(channel.get("cbw", None)),
                secondary80=_get_int(channel, "secondary80"),
            )

            bssid = connected.get("bssid")
            if not isinstance(bssid, list):
                raise TypeError(
                    f'Expected "bssid" to be list, got {type(bssid)}'
                )

            ssid = connected.get("ssid")
            if not isinstance(ssid, list):
                raise TypeError(f'Expected "ssid" to be list, got {type(ssid)}')

            protection = connected.get("protection")
            if not isinstance(protection, str):
                raise TypeError(
                    f'Expected "protection" to be str, got {type(protection)}'
                )

            return ClientStatusConnected(
                bssid=bssid,
                ssid=ssid,
                rssi_dbm=_get_int(connected, "rssi_dbm"),
                snr_db=_get_int(connected, "snr_db"),
                channel=wlan_channel,
                protection=Protection(
                    string_to_int_enum_map.get(protection, 0)
                ),
            )
