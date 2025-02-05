# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for wlan_using_fc.py"""

import copy
import unittest
from collections.abc import Iterator
from contextlib import contextmanager
from typing import TypeVar
from unittest import mock

import fidl.fuchsia_location_namedplace as f_location_namedplace
import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_common_security as f_wlan_common_security
import fidl.fuchsia_wlan_device_service as f_wlan_device_service
import fidl.fuchsia_wlan_ieee80211 as f_wlan_ieee80211
import fidl.fuchsia_wlan_sme as f_wlan_sme
from fuchsia_controller_py import Channel, ZxStatus

from honeydew.affordances.connectivity.wlan.utils.errors import (
    HoneydewWlanError,
)
from honeydew.affordances.connectivity.wlan.utils.types import (
    Authentication,
    BssDescription,
    BssType,
    ChannelBandwidth,
    ClientStatusConnected,
    ClientStatusConnecting,
    ClientStatusIdle,
    CountryCode,
    InformationElementType,
    Protection,
    QueryIfaceResponse,
    SecurityProtocol,
    WepCredentials,
    WlanChannel,
    WlanMacRole,
    WpaPassphraseCredentials,
    WpaPskCredentials,
)
from honeydew.affordances.connectivity.wlan.wlan import wlan_using_fc
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport

_TEST_SSID = "ThepromisedLAN"
_TEST_SSID_BYTES = list(str.encode(_TEST_SSID, "utf-8"))

_TEST_PASSWORD = "password"
_TEST_PSK = (
    "c9a68e83bfd123d144ec5256bc45682accfb8e8f0561f39f44dd388cba9e86f2".encode(
        "utf-8"
    )
)

_TEST_BSS_DESC_1_FC = f_wlan_common.BssDescription(
    bssid=[1, 2, 3],
    bss_type=f_wlan_common.BssType.PERSONAL,
    beacon_period=2,
    capability_info=3,
    ies=[InformationElementType.SSID, len(_TEST_SSID)] + _TEST_SSID_BYTES,
    channel=f_wlan_common.WlanChannel(
        primary=1,
        cbw=f_wlan_common.ChannelBandwidth.CBW20,
        secondary80=3,
    ),
    rssi_dbm=4,
    snr_db=5,
)
_TEST_BSS_DESC_1 = BssDescription(
    bssid=[1, 2, 3],
    bss_type=BssType.PERSONAL,
    beacon_period=2,
    capability_info=3,
    ies=[InformationElementType.SSID, len(_TEST_SSID)] + _TEST_SSID_BYTES,
    channel=WlanChannel(primary=1, cbw=ChannelBandwidth.CBW20, secondary80=3),
    rssi_dbm=4,
    snr_db=5,
)

# Use the same SSID such that the two scan results will be merged under the same
# SSID key, allowing the user to choose from multiple BSSes.
_TEST_BSS_DESC_2_FC = f_wlan_common.BssDescription(
    bssid=[3, 2, 1],
    bss_type=f_wlan_common.BssType.PERSONAL,
    beacon_period=5,
    capability_info=4,
    ies=[InformationElementType.SSID, len(_TEST_SSID)] + _TEST_SSID_BYTES,
    channel=f_wlan_common.WlanChannel(
        primary=2,
        cbw=f_wlan_common.ChannelBandwidth.CBW40,
        secondary80=4,
    ),
    rssi_dbm=3,
    snr_db=2,
)
_TEST_BSS_DESC_2 = BssDescription(
    bssid=[3, 2, 1],
    bss_type=BssType.PERSONAL,
    beacon_period=5,
    capability_info=4,
    ies=[InformationElementType.SSID, len(_TEST_SSID)] + _TEST_SSID_BYTES,
    channel=WlanChannel(primary=2, cbw=ChannelBandwidth.CBW40, secondary80=4),
    rssi_dbm=3,
    snr_db=2,
)

_TEST_QUERY_IFACE_RESP_FC = (
    f_wlan_device_service.DeviceMonitorQueryIfaceResult()
)
_TEST_QUERY_IFACE_RESP_FC.response = (
    f_wlan_device_service.DeviceMonitorQueryIfaceResponse(
        resp=f_wlan_device_service.QueryIfaceResponse(
            role=f_wlan_common.WlanMacRole.CLIENT,
            id=1,
            phy_id=1,
            phy_assigned_id=1,
            sta_addr=bytes([1, 2, 3, 4, 5, 6]),
        )
    )
)
_TEST_QUERY_IFACE_RESP = QueryIfaceResponse(
    role=WlanMacRole.CLIENT,
    id=1,
    phy_id=1,
    phy_assigned_id=1,
    sta_addr=[1, 2, 3, 4, 5, 6],
)

_TEST_SERVING_AP_INFO = f_wlan_sme.ServingApInfo(
    bssid=bytes([1, 2, 3, 4, 5, 6]),
    ssid=_TEST_SSID_BYTES,
    rssi_dbm=4,
    snr_db=5,
    channel=f_wlan_common.WlanChannel(
        primary=1,
        cbw=f_wlan_common.ChannelBandwidth.CBW20,
        secondary80=3,
    ),
    protection=f_wlan_sme.Protection.WPA2_PERSONAL,
)
_TEST_CLIENT_STATUS_CONNECTED = ClientStatusConnected(
    bssid=[1, 2, 3, 4, 5, 6],
    ssid=_TEST_SSID_BYTES,
    rssi_dbm=4,
    snr_db=5,
    channel=WlanChannel(primary=1, cbw=ChannelBandwidth.CBW20, secondary80=3),
    protection=Protection.WPA2_PERSONAL,
)

_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


async def _async_error(err: Exception) -> None:
    raise err


# pylint: disable=protected-access
class WlanFCTests(unittest.TestCase):
    """Unit tests for wlan_using_fc.py."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice,
            autospec=True,
        )
        self.fuchsia_device_close_obj = mock.MagicMock(
            spec=affordances_capable.FuchsiaDeviceClose,
            autospec=True,
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController,
            autospec=True,
        )
        self.fuchsia_device_close_obj = mock.MagicMock(
            spec=affordances_capable.FuchsiaDeviceClose,
            autospec=True,
        )
        self.ffx_transport_obj = mock.MagicMock(
            spec=ffx_transport.FFX,
            autospec=True,
        )

        self.ffx_transport_obj.run.return_value = "".join(
            wlan_using_fc._REQUIRED_CAPABILITIES
        )

        self.wlan_obj = wlan_using_fc.Wlan(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
            fuchsia_device_close=self.fuchsia_device_close_obj,
        )

    def tearDown(self) -> None:
        self.wlan_obj.close()
        super().tearDown()

    def test_verify_supported(self) -> None:
        """Test if verify_supported() works."""
        self.ffx_transport_obj.run.return_value = ""
        with self.assertRaises(NotSupportedError):
            self.wlan_obj = wlan_using_fc.Wlan(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
                fuchsia_device_close=self.fuchsia_device_close_obj,
            )

    def _mock_list_ifaces(self, zx_err: int | None = None) -> None:
        """Mock fuchsia.wlan.device.service.DeviceMonitor/QueryIface."""
        if zx_err:
            self.wlan_obj._device_monitor_proxy.list_ifaces.return_value = (
                _async_error(ZxStatus(zx_err))
            )
            return

        self.wlan_obj._device_monitor_proxy.list_ifaces.return_value = (
            _async_response(
                f_wlan_device_service.DeviceMonitorListIfacesResponse(
                    iface_list=[1]
                )
            )
        )

    def _mock_query_iface(self, zx_err: int | None = None) -> None:
        """Mock fuchsia.wlan.device.service.DeviceMonitor/QueryIface."""
        if zx_err:
            self.wlan_obj._device_monitor_proxy.query_iface.return_value = (
                _async_error(ZxStatus(zx_err))
            )
            return

        self.wlan_obj._device_monitor_proxy.query_iface.return_value = (
            _async_response(_TEST_QUERY_IFACE_RESP_FC)
        )

    @contextmanager
    def _mock_client_sme(self) -> Iterator[mock.MagicMock]:
        """Mock fuchsia.wlan.sme.ClientSme for the duration of this context."""
        client_sme = mock.MagicMock(spec=f_wlan_sme.ClientSme.Client)
        with mock.patch(
            "fidl.fuchsia_wlan_sme.ClientSme", autospec=True
        ) as f_client_sme:
            f_client_sme.Client.return_value = client_sme
            self.wlan_obj._device_monitor_proxy.get_client_sme.return_value = (
                _async_response(None)
            )
            yield client_sme

    def test_init_register_for_on_device_boot(self) -> None:
        """Test if Wlan registers on_device_boot."""
        self.reboot_affordance_obj.register_for_on_device_boot.assert_called_once_with(
            self.wlan_obj._connect_proxy
        )

    def test_init_connect_proxy(self) -> None:
        """Test if Wlan connects to WLAN proxies."""
        self.assertIsNotNone(self.wlan_obj._device_monitor_proxy)

    def test_connect_passes(self) -> None:
        """Verify connect works with multiple authentication modes."""
        for msg, auth in [
            ("open", Authentication(SecurityProtocol.OPEN, None)),
            (
                "wep",
                Authentication(
                    SecurityProtocol.WEP, WepCredentials(_TEST_PASSWORD)
                ),
            ),
            (
                "wpa1+passphrase",
                Authentication(
                    SecurityProtocol.WPA1,
                    WpaPassphraseCredentials(_TEST_PASSWORD),
                ),
            ),
            (
                "wpa1+psk",
                Authentication(
                    SecurityProtocol.WPA1, WpaPskCredentials(_TEST_PSK)
                ),
            ),
            (
                "wpa2+passphrase",
                Authentication(
                    SecurityProtocol.WPA2_PERSONAL,
                    WpaPassphraseCredentials(_TEST_PASSWORD),
                ),
            ),
            (
                "wpa2+psk",
                Authentication(
                    SecurityProtocol.WPA2_PERSONAL, WpaPskCredentials(_TEST_PSK)
                ),
            ),
        ]:
            with self.subTest(msg=msg, auth=auth):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:

                    def connect(
                        req: f_wlan_sme.ConnectRequest, txn: int
                    ) -> None:
                        expect = f_wlan_sme.ConnectRequest(
                            ssid=_TEST_SSID_BYTES,
                            bss_description=_TEST_BSS_DESC_1_FC,
                            multiple_bss_candidates=False,
                            # pylint: disable-next=cell-var-from-loop
                            authentication=auth.to_fidl(),
                            deprecated_scan_type=f_wlan_common.ScanType.ACTIVE,
                        )
                        self.assertListEqual(req.ssid, expect.ssid)
                        self.assertListEqual(
                            req.bss_description.bssid,
                            expect.bss_description.bssid,
                        )
                        self.assertEqual(
                            req.authentication, expect.authentication
                        )
                        server = f_wlan_sme.ConnectTransaction.Server(
                            Channel(txn)
                        )
                        server.on_connect_result(
                            result=f_wlan_sme.ConnectResult(
                                code=f_wlan_ieee80211.StatusCode.SUCCESS,
                                is_credential_rejected=False,
                                is_reconnect=False,
                            )
                        )

                    client_sme.connect = mock.Mock(wraps=connect)

                    status_resp = f_wlan_sme.ClientStatusResponse()
                    status_resp.connected = _TEST_SERVING_AP_INFO
                    client_sme.status.return_value = _async_response(
                        f_wlan_sme.ClientSmeStatusResponse(resp=status_resp)
                    )

                    self.wlan_obj.connect(
                        _TEST_SSID,
                        None,
                        _TEST_BSS_DESC_1,
                        auth,
                    )

    def test_connect_fails_no_authentication(self) -> None:
        """Verify connect fails without authentication."""
        with self.assertRaises(TypeError):
            self.wlan_obj.connect(
                _TEST_SSID,
                None,
                _TEST_BSS_DESC_1,
                authentication=None,
            )

    def test_connect_fails_sme_connect(self) -> None:
        """Verify connect fails when ClientSme.Connect() errors."""
        self.wlan_obj._device_monitor_proxy = mock.MagicMock(
            spec=f_wlan_device_service.DeviceMonitor.Client
        )
        self._mock_list_ifaces()
        self._mock_query_iface()

        with self._mock_client_sme() as client_sme:
            client_sme.connect.side_effect = [
                ZxStatus(ZxStatus.ZX_ERR_INTERNAL)
            ]
            with self.assertRaises(HoneydewWlanError):
                self.wlan_obj.connect(
                    _TEST_SSID,
                    None,
                    _TEST_BSS_DESC_1,
                    Authentication(SecurityProtocol.OPEN, None),
                )

    def test_connect_fails_connect_timeout(self) -> None:
        """Verify connect fails when ClientSme.Connect() takes too long."""
        self.wlan_obj._device_monitor_proxy = mock.MagicMock(
            spec=f_wlan_device_service.DeviceMonitor.Client
        )
        self._mock_list_ifaces()
        self._mock_query_iface()

        with self._mock_client_sme() as client_sme:
            client_sme.connect.side_effect = [None]
            with mock.patch("asyncio.wait_for", side_effect=[TimeoutError()]):
                with self.assertRaises(HoneydewWlanError):
                    self.wlan_obj.connect(
                        _TEST_SSID,
                        None,
                        _TEST_BSS_DESC_1,
                        Authentication(SecurityProtocol.OPEN, None),
                    )

    def test_connect_fails_driver_error(self) -> None:
        """Verify connect fails when the driver returns an error."""
        for msg, code, is_credentials_rejected in [
            (
                "error code",
                f_wlan_ieee80211.StatusCode.REFUSED_REASON_UNSPECIFIED,
                False,
            ),
            (
                "credential rejected",
                f_wlan_ieee80211.StatusCode.SUCCESS,
                True,
            ),
        ]:
            with self.subTest(
                msg=msg,
                code=code,
                is_credentials_rejected=is_credentials_rejected,
            ):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:

                    def connect(
                        # pylint: disable-next=unused-argument
                        req: f_wlan_sme.ConnectRequest,
                        txn: int,
                    ) -> None:
                        server = f_wlan_sme.ConnectTransaction.Server(
                            Channel(txn)
                        )
                        server.on_connect_result(
                            result=f_wlan_sme.ConnectResult(
                                # pylint: disable-next=cell-var-from-loop
                                code=code,
                                # pylint: disable-next=cell-var-from-loop
                                is_credential_rejected=is_credentials_rejected,
                                is_reconnect=False,
                            )
                        )

                    client_sme.connect = mock.Mock(wraps=connect)

                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.connect(
                            _TEST_SSID,
                            None,
                            _TEST_BSS_DESC_1,
                            Authentication(SecurityProtocol.OPEN, None),
                        )

    def test_connect_fails_client_status_wrong_ssid(self) -> None:
        """Verify connect fails when status() returns the wrong ssid."""
        self.wlan_obj._device_monitor_proxy = mock.MagicMock(
            spec=f_wlan_device_service.DeviceMonitor.Client
        )
        self._mock_list_ifaces()
        self._mock_query_iface()

        with self._mock_client_sme() as client_sme:

            def connect(req: f_wlan_sme.ConnectRequest, txn: int) -> None:
                expect = f_wlan_sme.ConnectRequest(
                    ssid=_TEST_SSID_BYTES,
                    bss_description=_TEST_BSS_DESC_1_FC,
                    multiple_bss_candidates=False,
                    authentication=Authentication(
                        SecurityProtocol.OPEN, None
                    ).to_fidl(),
                    deprecated_scan_type=f_wlan_common.ScanType.ACTIVE,
                )
                self.assertListEqual(req.ssid, expect.ssid)
                self.assertListEqual(
                    req.bss_description.bssid, expect.bss_description.bssid
                )
                self.assertEqual(req.authentication, expect.authentication)
                server = f_wlan_sme.ConnectTransaction.Server(Channel(txn))
                server.on_connect_result(
                    result=f_wlan_sme.ConnectResult(
                        code=f_wlan_ieee80211.StatusCode.SUCCESS,
                        is_credential_rejected=False,
                        is_reconnect=False,
                    )
                )

            client_sme.connect = mock.Mock(wraps=connect)

            status_resp = f_wlan_sme.ClientStatusResponse()
            status_resp.connected = copy.deepcopy(_TEST_SERVING_AP_INFO)
            status_resp.connected.ssid.append(ord("2"))
            client_sme.status.return_value = _async_response(
                f_wlan_sme.ClientSmeStatusResponse(resp=status_resp)
            )

            with self.assertRaises(HoneydewWlanError):
                self.wlan_obj.connect(
                    _TEST_SSID,
                    None,
                    _TEST_BSS_DESC_1,
                    Authentication(SecurityProtocol.OPEN, None),
                )

    def test_connect_fails_client_status_error(self) -> None:
        """Verify connect fails when status() returns an erroneous value."""
        resp_wrong_ssid = f_wlan_sme.ClientStatusResponse()
        resp_wrong_ssid.connected = copy.deepcopy(_TEST_SERVING_AP_INFO)
        resp_wrong_ssid.connected.ssid.append(ord("2"))

        for msg, status_resp in [
            (
                "wrong ssid",
                _async_response(
                    f_wlan_sme.ClientSmeStatusResponse(resp=resp_wrong_ssid)
                ),
            ),
            (
                "internal error",
                _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL)),
            ),
        ]:
            with self.subTest(msg=msg, status_resp=status_resp):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:

                    def connect(
                        req: f_wlan_sme.ConnectRequest, txn: int
                    ) -> None:
                        expect = f_wlan_sme.ConnectRequest(
                            ssid=_TEST_SSID_BYTES,
                            bss_description=_TEST_BSS_DESC_1_FC,
                            multiple_bss_candidates=False,
                            authentication=Authentication(
                                SecurityProtocol.OPEN, None
                            ).to_fidl(),
                            deprecated_scan_type=f_wlan_common.ScanType.ACTIVE,
                        )
                        self.assertListEqual(req.ssid, expect.ssid)
                        self.assertListEqual(
                            req.bss_description.bssid,
                            expect.bss_description.bssid,
                        )
                        self.assertEqual(
                            req.authentication, expect.authentication
                        )
                        server = f_wlan_sme.ConnectTransaction.Server(
                            Channel(txn)
                        )
                        server.on_connect_result(
                            result=f_wlan_sme.ConnectResult(
                                code=f_wlan_ieee80211.StatusCode.SUCCESS,
                                is_credential_rejected=False,
                                is_reconnect=False,
                            )
                        )

                    client_sme.connect = mock.Mock(wraps=connect)
                    client_sme.status.return_value = status_resp

                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.connect(
                            _TEST_SSID,
                            None,
                            _TEST_BSS_DESC_1,
                            Authentication(SecurityProtocol.OPEN, None),
                        )

    def test_create_iface(self) -> None:
        """Test if create_iface creates WLAN interfaces successfully."""
        for phy_id, sta_addr, role in [
            (1, "12:34:56:78:90:ab", WlanMacRole.CLIENT),
            (2, "12:34:56:78:90:ab", WlanMacRole.AP),
            (3, "12:34:56:78:90:ab", WlanMacRole.MESH),
            (4, None, WlanMacRole.CLIENT),
        ]:
            with self.subTest(phy_id=phy_id, sta_addr=sta_addr, role=role):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )

                self.wlan_obj._device_monitor_proxy.create_iface.return_value = _async_response(
                    f_wlan_device_service.DeviceMonitorCreateIfaceResponse(
                        iface_id=phy_id,
                    )
                )
                self.assertEqual(
                    self.wlan_obj.create_iface(
                        phy_id=phy_id, role=role, sta_addr=sta_addr
                    ),
                    phy_id,
                )

    def test_create_iface_invalid_mac(self) -> None:
        """Test if create_iface errors on invalid MAC."""
        for msg, phy_id, sta_addr in [
            ("not defined", 1, ""),
            ("too short", 2, "12:34:56:78:90"),
            ("invalid byte", 2, "12:34:56:78:90:abcd"),
            ("too long", 3, "12:34:56:78:90:ab:"),
        ]:
            with self.subTest(msg=msg, phy_id=phy_id, sta_addr=sta_addr):
                with self.assertRaises(ValueError):
                    self.wlan_obj.create_iface(
                        phy_id, WlanMacRole.CLIENT, sta_addr
                    )

    def test_destroy_iface(self) -> None:
        """Test if destroy_iface works."""
        for msg, iface_id, status in [
            ("valid", 1, ZxStatus.ZX_OK),
            ("invalid", 2, ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, iface_id=iface_id, status=status):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self.wlan_obj._device_monitor_proxy.destroy_iface.return_value = _async_response(
                    f_wlan_device_service.DeviceMonitorDestroyIfaceResponse(
                        status=status
                    )
                )

                if status == ZxStatus.ZX_OK:
                    self.wlan_obj.destroy_iface(iface_id)
                else:
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.destroy_iface(iface_id)

    def test_disconnect(self) -> None:
        """Test if disconnect works."""
        for msg, zx_err in [
            ("valid", None),
            ("invalid", ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, zx_err=zx_err):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:
                    if not zx_err:
                        client_sme.disconnect.return_value = _async_response(
                            f_wlan_sme.Empty()
                        )
                        self.wlan_obj.disconnect()
                    else:
                        client_sme.disconnect.return_value = _async_error(
                            ZxStatus(zx_err)
                        )
                        with self.assertRaises(HoneydewWlanError):
                            self.wlan_obj.disconnect()

    def test_get_iface_id_list(self) -> None:
        """Test if get_iface_id_list works."""
        for msg, zx_err in [
            ("valid", None),
            ("invalid", ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, zx_err=zx_err):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )

                self._mock_list_ifaces(zx_err)
                if not zx_err:
                    self.wlan_obj.get_iface_id_list()
                else:
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.get_iface_id_list()

    def test_get_country(self) -> None:
        """Test if get_country works."""
        for msg, country_code, zx_err, expected, expected_err in [
            ("valid - WW", [87, 87], None, CountryCode.WORLDWIDE, None),
            (
                "valid - US",
                [85, 83],
                None,
                CountryCode.UNITED_STATES_OF_AMERICA,
                None,
            ),
            ("invalid - unknown", [0, 0], None, None, ValueError),
            ("invalid - empty", [], None, None, ValueError),
            (
                "invalid - internal error",
                [87, 87],
                ZxStatus.ZX_ERR_INTERNAL,
                None,
                HoneydewWlanError,
            ),
        ]:
            with self.subTest(
                msg=msg,
                country_code=country_code,
                zx_err=zx_err,
                expected=expected,
                expected_err=expected_err,
            ):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )

                if zx_err:
                    self.wlan_obj._device_monitor_proxy.get_country.return_value = _async_error(
                        ZxStatus(zx_err)
                    )
                else:
                    res = f_wlan_device_service.DeviceMonitorGetCountryResult()
                    res.response = (
                        f_wlan_device_service.DeviceMonitorGetCountryResponse(
                            resp=f_wlan_device_service.GetCountryResponse(
                                alpha2=country_code
                            )
                        )
                    )
                    self.wlan_obj._device_monitor_proxy.get_country.return_value = _async_response(
                        res
                    )

                if expected_err:
                    with self.assertRaises(expected_err):
                        self.wlan_obj.get_country(1)
                else:
                    got = self.wlan_obj.get_country(1)
                    self.assertEqual(got, expected)

    def test_get_phy_id_list(self) -> None:
        """Test if get_phy_id_list works."""
        for msg, zx_err in [
            ("valid", None),
            ("invalid", ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, zx_err=zx_err):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )

                if not zx_err:
                    self.wlan_obj._device_monitor_proxy.list_phys.return_value = _async_response(
                        f_wlan_device_service.DeviceMonitorListPhysResponse(
                            phy_list=[1]
                        )
                    )
                    self.wlan_obj.get_phy_id_list()
                else:
                    self.wlan_obj._device_monitor_proxy.list_phys.return_value = _async_error(
                        ZxStatus(zx_err)
                    )
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.get_phy_id_list()

    def test_query_iface(self) -> None:
        """Test if query_iface works."""
        for msg, zx_err in [
            ("valid", None),
            ("invalid", ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, zx_err=zx_err):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )

                self._mock_query_iface(zx_err)
                if not zx_err:
                    got = self.wlan_obj.query_iface(1)
                    self.assertEqual(got, _TEST_QUERY_IFACE_RESP)
                else:
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_obj.query_iface(1)

    def test_scan_for_bss_info(self) -> None:
        """Test if scan_for_bss_info works."""
        for msg, err, zx_err in [
            ("valid", None, None),
            (
                "invalid - should wait",
                f_wlan_sme.ScanErrorCode.SHOULD_WAIT,
                None,
            ),
            ("invalid - internal", None, ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, err=err, zx_err=zx_err):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:
                    if zx_err:
                        client_sme.scan_for_controller.return_value = (
                            _async_error(ZxStatus(zx_err))
                        )
                    else:
                        compatibility = f_wlan_sme.Compatibility()
                        compatibility.compatible = (
                            f_wlan_common_security.Protocol.WPA2_PERSONAL
                        )
                        res = f_wlan_sme.ClientSmeScanForControllerResult()
                        res.response = (
                            f_wlan_sme.ClientSmeScanForControllerResponse(
                                scan_results=[
                                    f_wlan_sme.ScanResult(
                                        compatibility=compatibility,
                                        timestamp_nanos=0,
                                        bss_description=_TEST_BSS_DESC_1_FC,
                                    ),
                                    f_wlan_sme.ScanResult(
                                        compatibility=compatibility,
                                        timestamp_nanos=0,
                                        bss_description=_TEST_BSS_DESC_2_FC,
                                    ),
                                ],
                            )
                        )
                        res.err = err
                        client_sme.scan_for_controller.return_value = (
                            _async_response(res)
                        )

                    if err or zx_err:
                        with self.assertRaises(HoneydewWlanError):
                            self.wlan_obj.scan_for_bss_info()
                    else:
                        scan_results = self.wlan_obj.scan_for_bss_info()
                        self.assertEqual(
                            scan_results,
                            {_TEST_SSID: [_TEST_BSS_DESC_1, _TEST_BSS_DESC_2]},
                        )

    def test_set_region(self) -> None:
        """Test if set_region works."""
        for msg, zx_err in [
            ("valid", None),
            ("invalid", ZxStatus.ZX_ERR_INTERNAL),
        ]:
            with self.subTest(msg=msg, zx_err=zx_err):
                regulatory_mock = mock.MagicMock(
                    spec=f_location_namedplace.RegulatoryRegionConfigurator.Client
                )
                with mock.patch(
                    "fidl.fuchsia_location_namedplace.RegulatoryRegionConfigurator",
                    autospec=True,
                ) as f_regulatory_mock:
                    f_regulatory_mock.Client.return_value = regulatory_mock

                    if not zx_err:
                        regulatory_mock.set_region.return_value = None
                        self.wlan_obj.set_region("AT")
                    else:
                        regulatory_mock.set_region.side_effect = ZxStatus(
                            zx_err
                        )
                        with self.assertRaises(HoneydewWlanError):
                            self.wlan_obj.set_region("AT")

    def test_status(self) -> None:
        """Test if status works."""

        def make_noop(_: f_wlan_sme.ClientStatusResponse) -> None:
            pass

        def make_idle(r: f_wlan_sme.ClientStatusResponse) -> None:
            r.idle = f_wlan_sme.Empty()

        def make_connected(r: f_wlan_sme.ClientStatusResponse) -> None:
            r.connected = _TEST_SERVING_AP_INFO

        def make_connecting(r: f_wlan_sme.ClientStatusResponse) -> None:
            r.connecting = _TEST_SSID_BYTES

        for msg, make_resp, zx_err, expected in [
            ("valid - idle", make_idle, None, ClientStatusIdle()),
            (
                "valid - connected",
                make_connected,
                None,
                _TEST_CLIENT_STATUS_CONNECTED,
            ),
            (
                "valid - connecting",
                make_connecting,
                None,
                ClientStatusConnecting(_TEST_SSID_BYTES),
            ),
            ("invalid", make_noop, ZxStatus.ZX_ERR_INTERNAL, None),
        ]:
            with self.subTest(
                msg=msg, make_resp=make_resp, zx_err=zx_err, expected=expected
            ):
                self.wlan_obj._device_monitor_proxy = mock.MagicMock(
                    spec=f_wlan_device_service.DeviceMonitor.Client
                )
                self._mock_list_ifaces()
                self._mock_query_iface()

                with self._mock_client_sme() as client_sme:
                    if not zx_err:
                        resp = f_wlan_sme.ClientStatusResponse()
                        make_resp(resp)
                        client_sme.status.return_value = _async_response(
                            f_wlan_sme.ClientSmeStatusResponse(resp=resp)
                        )
                        got = self.wlan_obj.status()
                        self.assertEqual(got, expected)
                    else:
                        client_sme.status.return_value = _async_error(
                            ZxStatus(zx_err)
                        )
                        with self.assertRaises(HoneydewWlanError):
                            self.wlan_obj.status()


if __name__ == "__main__":
    unittest.main()
