#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan affordance implementation using SL4F."""

import enum
import logging
from collections.abc import Mapping

from honeydew.interfaces.affordances.wlan import wlan
from honeydew.transports.sl4f import SL4F
from honeydew.typing.wlan import (
    BssDescription,
    BssType,
    ChannelBandwidth,
    ClientStatusResponse,
    CountryCodeList,
    Protection,
    QueryIfaceResponse,
    ServingApInfo,
    WlanChannel,
    WlanMacRole,
)

_LOGGER: logging.Logger = logging.getLogger(__name__)


def _get_int(m: Mapping[str, object], key: str) -> int:
    val = m[key]
    if not isinstance(val, int):
        raise TypeError(f'Expected "{val}" to be int, got {type(val)}')
    return val


class _Sl4fMethods(enum.StrEnum):
    """Sl4f server commands."""

    CONNECT = "wlan.connect"
    CREATE_IFACE = "wlan.create_iface"
    DESTROY_IFACE = "wlan.destroy_iface"
    DISCONNECT = "wlan.disconnect"
    GET_COUNTRY = "wlan.get_country"
    GET_IFACE_ID_LIST = "wlan.get_iface_id_list"
    GET_PHY_ID_LIST = "wlan.get_phy_id_list"
    QUERY_IFACE = "wlan.query_iface"
    SCAN_FOR_BSS_INFO = "wlan.scan_for_bss_info"
    SET_REGION = "wlan.set_region"
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

    # List all the public methods
    def connect(
        self,
        ssid: str,
        password: str | None,
        bss_desc: BssDescription,
    ) -> bool:
        """Trigger connection to a network.

        Args:
            ssid: The network to connect to.
            password: The password for the network.
            bss_desc: The basic service set for target network.

        Returns:
            True on success otherwise false.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return value not a bool.
        """
        method_params = {
            "target_ssid": ssid,
            "target_pwd": password,
            "target_bss_desc": bss_desc,
        }
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.CONNECT, params=method_params
        )
        result = resp.get("result", False)

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
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return value not an int.
        """
        method_params = {
            "phy_id": phy_id,
            "role": role,
            "sta_addr": sta_addr,
        }
        resp = self._sl4f.run(
            method=_Sl4fMethods.CREATE_IFACE, params=method_params
        )

        return _get_int(resp, "result")

    def destroy_iface(self, iface_id: int) -> None:
        """Destroy WLAN interface by ID.

        Args:
            iface_id: The interface to destroy.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        method_params = {"iface_id": iface_id}
        self._sl4f.run(method=_Sl4fMethods.DESTROY_IFACE, params=method_params)

    def disconnect(self) -> None:
        """Disconnect any current wifi connections.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        self._sl4f.run(method=_Sl4fMethods.DISCONNECT)

    def get_country(self, phy_id: int) -> CountryCodeList:
        """Queries the currently configured country code from phy `phy_id`.

        Args:
            phy_id: A phy id that is present on the device.

        Returns:
            The currently configured country code from `phy_id`.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: Return value not a list.
        """
        method_params = {"phy_id": phy_id}

        resp = self._sl4f.run(
            method=_Sl4fMethods.GET_COUNTRY, params=method_params
        )
        result = resp.get("result", "")

        if not isinstance(result, str):
            raise TypeError(f'Expected "result" to be str, got {type(result)}')

        return CountryCodeList(result)

    def get_iface_id_list(self) -> list[int]:
        """Get list of wlan iface IDs on device.

        Returns:
            A list of wlan iface IDs that are present on the device.

        Raises:
            errors.Sl4fError: On failure.
            TypeError: Return value not a list.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.GET_IFACE_ID_LIST
        )
        result: object = resp.get("result", [])

        if not isinstance(result, list):
            raise TypeError(f'Expected "result" to be list, got {type(result)}')

        return result

    def get_phy_id_list(self) -> list[int]:
        """Get list of phy ids on device.

        Returns:
            A list of phy ids that is present on the device.

        Raises:
            errors.Sl4fError: On failure.
            TypeError: Return value not a list.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.GET_PHY_ID_LIST
        )
        result: object = resp.get("result", [])

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
            errors.Sl4fError: On failure.
            TypeError: If any of the return values are not of the expected type.
        """
        method_params = {"iface_id": iface_id}

        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.QUERY_IFACE, params=method_params
        )
        result: object = resp.get("result", {})

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        if not isinstance(result["sta_addr"], list):
            raise TypeError(
                'Expected "sta_addr" to be list, '
                f'got {type(result["sta_addr"])}'
            )

        return QueryIfaceResponse(
            role=WlanMacRole(result["role"]),
            id=_get_int(result, "id"),
            phy_id=_get_int(result, "phy_id"),
            phy_assigned_id=_get_int(result, "phy_assigned_id"),
            sta_addr=result["sta_addr"],
        )

    def scan_for_bss_info(self) -> dict[str, BssDescription]:
        """Scans and returns BSS info.

        Returns:
            A dict mapping each seen SSID to a list of BSS Description IE
            blocks, one for each BSS observed in the network

        Raises:
            errors.Sl4fError: Sl4f run command failed.
            TypeError: If any of the return values are not of the expected type.
        """
        resp: dict[str, object] = self._sl4f.run(
            method=_Sl4fMethods.SCAN_FOR_BSS_INFO
        )
        result: object = resp.get("result")

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        bss_descriptions = {}
        for key, bss in result.items():
            if not isinstance(bss, dict):
                raise TypeError(
                    f'Expected "bss_block" to be dict, got {type(bss)}'
                )

            if not isinstance(bss["bssid"], list):
                raise TypeError(
                    f'Expected "bssid" to be list, got {type(bss["bssid"])}'
                )

            if not isinstance(bss["ies"], list):
                raise TypeError(
                    f'Expected "ies" to be list, got {type(bss["ies"])}'
                )

            channel = WlanChannel(
                primary=_get_int(bss["channel"], "primary"),
                cbw=ChannelBandwidth(bss["channel"]["cbw"]),
                secondary80=_get_int(bss["channel"], "secondary80"),
            )

            bss_block = BssDescription(
                bssid=bss["bssid"],
                bss_type=BssType(bss["bss_type"]),
                beacon_period=_get_int(bss, "beacon_period"),
                capability_info=_get_int(bss, "capability_info"),
                ies=bss["ies"],
                channel=channel,
                rssi_dbm=_get_int(bss, "rssi_dbm"),
                snr_db=_get_int(bss, "snr_db"),
            )

            bss_descriptions[key] = bss_block

        return bss_descriptions

    def set_region(self, region_code: str) -> None:
        """Set regulatory region.

        Args:
            region_code: 2-byte ASCII string.

        Raises:
            errors.Sl4fError: Sl4f run command failed.
        """
        method_params = {"region_code": region_code}
        self._sl4f.run(method=_Sl4fMethods.SET_REGION, params=method_params)

    def status(self) -> ClientStatusResponse:
        """Request connection status

        Returns:
            ClientStatusResponse state summary and
            status of various networks connections.

        Raises:
            errors.Sl4fError: On failure.
            TypeError: If any of the return values are not of the expected type.
        """
        resp: dict[str, object] = self._sl4f.run(method=_Sl4fMethods.STATUS)
        result: object = resp.get("result", None)

        if not isinstance(result, dict):
            raise TypeError(f'Expected "result" to be dict, got {type(result)}')

        if not isinstance(result["Connected"], dict):
            raise TypeError(
                'Expected "Connected" to be dict,'
                f'got {type(result["Connected"])}'
            )

        if not isinstance(result["Connecting"], list):
            raise TypeError(
                'Expected "Connecting" to be list, '
                f'got {type(result["Connecting"])}'
            )

        channel = WlanChannel(
            primary=result["Connected"]["channel"]["primary"],
            cbw=ChannelBandwidth(result["Connected"]["channel"]["cbw"]),
            secondary80=result["Connected"]["channel"]["secondary80"],
        )

        if not isinstance(result["Connected"]["bssid"], list):
            raise TypeError(
                'Expected "bssid" to be list, '
                f'got {type(result["Connected"]["bssid"])}'
            )

        if not isinstance(result["Connected"]["ssid"], list):
            raise TypeError(
                'Expected "ssid" to be list, '
                f'got {type(result["Connected"]["ssid"])}'
            )

        serving_ap_info = ServingApInfo(
            bssid=result["Connected"]["bssid"],
            ssid=result["Connected"]["ssid"],
            rssi_dbm=_get_int(result["Connected"], "rssi_dbm"),
            snr_db=_get_int(result["Connected"], "snr_db"),
            channel=channel,
            protection=Protection(result["Connected"]["protection"]),
        )

        return ClientStatusResponse(
            connected=serving_ap_info,
            connecting=result["Connecting"],
            idle=result["Idle"],
        )
