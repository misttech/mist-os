# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for wlan_policy_using_fc.py"""

import asyncio
import unittest
from collections.abc import Iterator
from contextlib import contextmanager
from typing import TypeVar
from unittest import mock

import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_policy as f_wlan_policy
from fuchsia_controller_py import Channel, ZxStatus

from honeydew.affordances.connectivity.wlan.utils.errors import (
    HoneydewWlanError,
)
from honeydew.affordances.connectivity.wlan.utils.types import (
    ClientStateSummary,
    ConnectionState,
    NetworkConfig,
    NetworkIdentifier,
    NetworkState,
    RequestStatus,
    SecurityType,
    WlanClientState,
)
from honeydew.affordances.connectivity.wlan.wlan_policy import (
    wlan_policy_using_fc,
)
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport

_TEST_SSID = "ThepromisedLAN"
_TEST_SSID_BYTES = list(str.encode(_TEST_SSID))

_TEST_PASSWORD = "password"
_TEST_PSK = "c9a68e83bfd123d144ec5256bc45682accfb8e8f0561f39f44dd388cba9e86f2"

_TEST_CREDENTIAL_NONE = f_wlan_policy.Credential()
_TEST_CREDENTIAL_NONE.none = f_wlan_policy.Empty()

_TEST_CREDENTIAL_PASSWORD = f_wlan_policy.Credential()
_TEST_CREDENTIAL_PASSWORD.password = list(str.encode(_TEST_PASSWORD))

_TEST_CREDENTIAL_PSK = f_wlan_policy.Credential()
_TEST_CREDENTIAL_PSK.psk = list(bytes.fromhex(_TEST_PSK))

_TEST_NETWORK_CONFIG_NONE = NetworkConfig(
    ssid=_TEST_SSID,
    security_type=SecurityType.NONE,
    credential_type="None",
    credential_value="",
)
_TEST_NETWORK_CONFIG_NONE_FIDL = f_wlan_policy.NetworkConfig(
    id=f_wlan_policy.NetworkIdentifier(
        ssid=_TEST_SSID_BYTES,
        type=f_wlan_policy.SecurityType.NONE,
    ),
    credential=_TEST_CREDENTIAL_NONE,
)

_TEST_NETWORK_CONFIG_PASSWORD = NetworkConfig(
    ssid=_TEST_SSID,
    security_type=SecurityType.WPA2,
    credential_type="Password",
    credential_value=_TEST_PASSWORD,
)
_TEST_NETWORK_CONFIG_PASSWORD_FIDL = f_wlan_policy.NetworkConfig(
    id=f_wlan_policy.NetworkIdentifier(
        ssid=_TEST_SSID_BYTES,
        type=f_wlan_policy.SecurityType.WPA2,
    ),
    credential=_TEST_CREDENTIAL_PASSWORD,
)

_TEST_NETWORK_CONFIG_PSK = NetworkConfig(
    ssid=_TEST_SSID,
    security_type=SecurityType.WPA2,
    credential_type="Psk",
    credential_value=_TEST_PSK,
)
_TEST_NETWORK_CONFIG_PSK_FIDL = f_wlan_policy.NetworkConfig(
    id=f_wlan_policy.NetworkIdentifier(
        ssid=_TEST_SSID_BYTES,
        type=f_wlan_policy.SecurityType.WPA2,
    ),
    credential=_TEST_CREDENTIAL_PSK,
)

_TEST_MAC_ADDRESS_BYTES = bytes([1, 35, 69, 103, 137, 171])  # 01:23:45:67:89:ab


def _make_scan_result(ssid: str) -> f_wlan_policy.ScanResult:
    return f_wlan_policy.ScanResult(
        id=f_wlan_policy.NetworkIdentifier(
            ssid=list(ssid.encode("utf-8")),
            type=f_wlan_policy.SecurityType.WPA2,
        ),
        entries=[
            f_wlan_policy.Bss(
                bssid=list(_TEST_MAC_ADDRESS_BYTES),
                rssi=0,
                frequency=0,
                timestamp_nanos=0,
            ),
        ],
        compatibility=f_wlan_policy.Compatibility.SUPPORTED,
    )


_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


async def _async_error(err: Exception) -> None:
    raise err


# pylint: disable=protected-access
class WlanPolicyFCTests(unittest.TestCase):
    """Unit tests for wlan_policy_using_fc.py"""

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
        self.ffx_transport_obj = mock.MagicMock(
            spec=ffx_transport.FFX,
            autospec=True,
        )

        self.ffx_transport_obj.run.return_value = "".join(
            wlan_policy_using_fc._REQUIRED_CAPABILITIES
        )

        self.wlan_policy_obj = wlan_policy_using_fc.WlanPolicy(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
            fuchsia_device_close=self.fuchsia_device_close_obj,
        )
        self.client_state_updates_proxy: (
            f_wlan_policy.ClientStateUpdatesClient | None
        ) = None
        self.scan_result_iterator: asyncio.Task[None] | None = None
        self.network_config_iterator: asyncio.Task[None] | None = None

    def tearDown(self) -> None:
        self.wlan_policy_obj._close()
        return super().tearDown()

    @contextmanager
    def _mock_create_client_controller(self) -> Iterator[mock.MagicMock]:
        """Mock the creation of a fuchsia.wlan.policy/ClientController."""

        # Create a FIDL client to the ClientStateUpdates server.
        # pylint: disable-next=unused-argument
        def get_controller(requests: Channel, updates: Channel) -> None:
            self.client_state_updates_proxy = (
                f_wlan_policy.ClientStateUpdates.Client(updates)
            )

        self.wlan_policy_obj._client_provider_proxy.get_controller = mock.Mock(
            wraps=get_controller
        )

        client_controller_proxy = mock.MagicMock(
            spec=f_wlan_policy.ClientController.Client
        )
        with mock.patch(
            "fidl.fuchsia_wlan_policy.ClientController", autospec=True
        ) as f_client_controller:
            f_client_controller.Client.return_value = client_controller_proxy
            yield client_controller_proxy

    @contextmanager
    def _mock_client_listener(self) -> Iterator[mock.MagicMock]:
        """Mock the creation of a fuchsia.wlan.policy/ClientListener."""

        client_listener_proxy = mock.MagicMock(
            spec=f_wlan_policy.ClientListener.Client
        )

        # Create a FIDL client to the ClientListener server.
        def get_listener(updates: Channel) -> None:
            self.client_state_updates_proxy = (
                f_wlan_policy.ClientStateUpdates.Client(updates)
            )

        client_listener_proxy.get_listener = mock.Mock(wraps=get_listener)

        with mock.patch(
            "fidl.fuchsia_wlan_policy.ClientListener", autospec=True
        ) as f_client_listener:
            f_client_listener.Client.return_value = client_listener_proxy
            yield client_listener_proxy

    def test_verify_supported(self) -> None:
        """Test if verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.wlan_policy_obj = wlan_policy_using_fc.WlanPolicy(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
                fuchsia_device_close=self.fuchsia_device_close_obj,
            )

    def test_init_connect_proxy(self) -> None:
        """Test if WlanPolicy connects to WLAN Policy proxies."""
        self.assertIsNotNone(self.wlan_policy_obj._client_provider_proxy)

    def test_connect(self) -> None:
        """Test if connect works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            for msg, resp, expected in [
                (
                    "acknowledged",
                    _async_response(
                        f_wlan_policy.ClientControllerConnectResponse(
                            status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                        )
                    ),
                    RequestStatus.ACKNOWLEDGED,
                ),
                (
                    "not supported",
                    _async_response(
                        f_wlan_policy.ClientControllerConnectResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_NOT_SUPPORTED
                        )
                    ),
                    RequestStatus.REJECTED_NOT_SUPPORTED,
                ),
                (
                    "incompatible mode",
                    _async_response(
                        f_wlan_policy.ClientControllerConnectResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_INCOMPATIBLE_MODE
                        )
                    ),
                    RequestStatus.REJECTED_INCOMPATIBLE_MODE,
                ),
                (
                    "already in use",
                    _async_response(
                        f_wlan_policy.ClientControllerConnectResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_ALREADY_IN_USE
                        )
                    ),
                    RequestStatus.REJECTED_ALREADY_IN_USE,
                ),
                (
                    "duplicate request",
                    _async_response(
                        f_wlan_policy.ClientControllerConnectResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_DUPLICATE_REQUEST
                        )
                    ),
                    RequestStatus.REJECTED_DUPLICATE_REQUEST,
                ),
                (
                    "internal error",
                    _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL)),
                    None,
                ),
            ]:
                with self.subTest(msg=msg, resp=resp, expected=expected):
                    client_controller.connect.reset_mock()
                    client_controller.connect.return_value = resp
                    if expected:
                        connect_resp = self.wlan_policy_obj.connect(
                            _TEST_SSID, SecurityType.NONE
                        )
                        self.assertEqual(connect_resp, expected)
                    else:
                        with self.assertRaises(HoneydewWlanError):
                            self.wlan_policy_obj.connect(
                                _TEST_SSID, SecurityType.NONE
                            )
                    client_controller.connect.assert_called_once()

    def test_create_client_controller(self) -> None:
        """Test if create_client_controller works."""
        with self._mock_create_client_controller():
            self.wlan_policy_obj.create_client_controller()

            self.assertIsNotNone(self.client_state_updates_proxy)
            self.assertIsNotNone(self.wlan_policy_obj._client_controller)
            assert self.client_state_updates_proxy is not None
            assert self.wlan_policy_obj._client_controller is not None

            self.assertEqual(
                self.wlan_policy_obj._client_controller.updates.qsize(), 0
            )

            self.wlan_policy_obj.loop().run_until_complete(
                self.client_state_updates_proxy.on_client_state_update(
                    summary=f_wlan_policy.ClientStateSummary(
                        state=f_wlan_policy.WlanClientState.CONNECTIONS_ENABLED,
                        networks=[],
                    ),
                )
            )

            update = self.wlan_policy_obj.loop().run_until_complete(
                self.wlan_policy_obj._client_controller.updates.get()
            )
            self.assertEqual(
                update,
                ClientStateSummary(
                    state=WlanClientState.CONNECTIONS_ENABLED, networks=[]
                ),
            )

    def test_get_saved_networks(self) -> None:
        """Test if get_saved_networks works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            def get_saved_networks(iterator: int) -> None:
                server = TestNetworkConfigIteratorImpl(
                    Channel(iterator),
                    items=[
                        [
                            _TEST_NETWORK_CONFIG_NONE_FIDL,
                            _TEST_NETWORK_CONFIG_PASSWORD_FIDL,
                        ],
                        [_TEST_NETWORK_CONFIG_PSK_FIDL],
                    ],
                )
                self.network_config_iterator = (
                    self.wlan_policy_obj.loop().create_task(server.serve())
                )

            client_controller.get_saved_networks = mock.Mock(
                wraps=get_saved_networks
            )

            networks = self.wlan_policy_obj.get_saved_networks()
            self.assertEqual(
                networks,
                [
                    _TEST_NETWORK_CONFIG_NONE,
                    _TEST_NETWORK_CONFIG_PASSWORD,
                    _TEST_NETWORK_CONFIG_PSK,
                ],
            )

            assert self.network_config_iterator is not None
            self.wlan_policy_obj.cancel_task(self.network_config_iterator)

    def test_get_update(self) -> None:
        """Test if get_update works."""
        with self._mock_create_client_controller():
            self.wlan_policy_obj.create_client_controller()

            self.assertIsNotNone(self.client_state_updates_proxy)
            self.assertIsNotNone(self.wlan_policy_obj._client_controller)
            assert self.client_state_updates_proxy is not None
            assert self.wlan_policy_obj._client_controller is not None

            for msg, fidl, expected in [
                (
                    "enabled",
                    f_wlan_policy.ClientStateSummary(
                        state=f_wlan_policy.WlanClientState.CONNECTIONS_ENABLED,
                        networks=[],
                    ),
                    ClientStateSummary(
                        state=WlanClientState.CONNECTIONS_ENABLED, networks=[]
                    ),
                ),
                (
                    "connecting",
                    f_wlan_policy.ClientStateSummary(
                        state=f_wlan_policy.WlanClientState.CONNECTIONS_ENABLED,
                        networks=[
                            f_wlan_policy.NetworkState(
                                id=f_wlan_policy.NetworkIdentifier(
                                    ssid=list(b"Google Guest"),
                                    type=f_wlan_policy.SecurityType.WPA2,
                                ),
                                state=f_wlan_policy.ConnectionState.CONNECTING,
                                status=None,
                            ),
                        ],
                    ),
                    ClientStateSummary(
                        state=WlanClientState.CONNECTIONS_ENABLED,
                        networks=[
                            NetworkState(
                                network_identifier=NetworkIdentifier(
                                    ssid="Google Guest",
                                    security_type=SecurityType.WPA2,
                                ),
                                connection_state=ConnectionState.CONNECTING,
                                disconnect_status=None,
                            )
                        ],
                    ),
                ),
                (
                    "disabled",
                    f_wlan_policy.ClientStateSummary(
                        state=f_wlan_policy.WlanClientState.CONNECTIONS_DISABLED,
                        networks=[],
                    ),
                    ClientStateSummary(
                        state=WlanClientState.CONNECTIONS_DISABLED, networks=[]
                    ),
                ),
            ]:
                with self.subTest(msg=msg, fidl=fidl, expected=expected):
                    self.wlan_policy_obj.loop().run_until_complete(
                        self.client_state_updates_proxy.on_client_state_update(
                            summary=fidl,
                        )
                    )
                    self.assertEqual(
                        self.wlan_policy_obj.get_update(),
                        expected,
                    )

    def test_remove_all_networks(self) -> None:
        """Test if remove_all_networks works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            # Mock get_saved_networks
            def get_saved_networks(iterator: int) -> None:
                server = TestNetworkConfigIteratorImpl(
                    Channel(iterator),
                    items=[
                        [
                            _TEST_NETWORK_CONFIG_NONE_FIDL,
                            _TEST_NETWORK_CONFIG_PASSWORD_FIDL,
                            _TEST_NETWORK_CONFIG_PSK_FIDL,
                        ],
                    ],
                )
                self.network_config_iterator = (
                    self.wlan_policy_obj.loop().create_task(server.serve())
                )

            client_controller.get_saved_networks = mock.Mock(
                wraps=get_saved_networks
            )

            # Mock remove_network, which should be called once for each saved
            # network.
            res = f_wlan_policy.ClientControllerRemoveNetworkResult()
            res.response = f_wlan_policy.ClientControllerRemoveNetworkResponse()
            client_controller.remove_network.side_effect = [
                _async_response(res),
                _async_response(res),
                _async_response(res),
            ]

            # Remove all networks
            self.wlan_policy_obj.remove_all_networks()
            client_controller.remove_network.assert_has_calls(
                [
                    mock.call(config=_TEST_NETWORK_CONFIG_NONE_FIDL),
                    mock.call(config=_TEST_NETWORK_CONFIG_PASSWORD_FIDL),
                    mock.call(config=_TEST_NETWORK_CONFIG_PSK_FIDL),
                ]
            )

            # Cleanup
            assert self.network_config_iterator is not None
            self.wlan_policy_obj.cancel_task(self.network_config_iterator)

    def test_remove_network_passes(self) -> None:
        """Test if remove_network works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            res = f_wlan_policy.ClientControllerRemoveNetworkResult()
            res.response = f_wlan_policy.ClientControllerRemoveNetworkResponse()
            client_controller.remove_network.return_value = _async_response(res)

            self.wlan_policy_obj.remove_network(
                _TEST_SSID, SecurityType.NONE, None
            )
            client_controller.remove_network.assert_called_with(
                config=_TEST_NETWORK_CONFIG_NONE_FIDL
            )

    def test_remove_network_fails(self) -> None:
        """Test if remove_network throws HoneydewWlanError as expected."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            with self.subTest(msg="NetworkConfigChangeError"):
                res = f_wlan_policy.ClientControllerRemoveNetworkResult()
                res.err = (
                    f_wlan_policy.NetworkConfigChangeError.CREDENTIAL_LEN_ERROR
                )
                client_controller.remove_network.return_value = _async_response(
                    res
                )

                with self.assertRaises(HoneydewWlanError):
                    self.wlan_policy_obj.remove_network(
                        _TEST_SSID, SecurityType.NONE, None
                    )
                client_controller.remove_network.assert_called_once()

            with self.subTest(msg="ZxStatus"):
                res = f_wlan_policy.ClientControllerRemoveNetworkResult()
                res.err = (
                    f_wlan_policy.NetworkConfigChangeError.CREDENTIAL_LEN_ERROR
                )
                client_controller.remove_network.reset_mock()
                client_controller.remove_network.return_value = _async_error(
                    ZxStatus(ZxStatus.ZX_ERR_INTERNAL)
                )

                with self.assertRaises(HoneydewWlanError):
                    self.wlan_policy_obj.remove_network(
                        _TEST_SSID, SecurityType.NONE, None
                    )
                client_controller.remove_network.assert_called_once()

    def test_save_network_passes(self) -> None:
        """Test if save_network works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            res = f_wlan_policy.ClientControllerSaveNetworkResult()
            res.response = f_wlan_policy.ClientControllerSaveNetworkResponse()
            client_controller.save_network.return_value = _async_response(res)

            self.wlan_policy_obj.save_network(
                _TEST_SSID, SecurityType.NONE, None
            )
            client_controller.save_network.assert_called_once()

    def test_save_network_fails(self) -> None:
        """Test if save_network throws HoneydewWlanError as expected."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            with self.subTest(msg="NetworkConfigChangeError"):
                res = f_wlan_policy.ClientControllerSaveNetworkResult()
                res.err = (
                    f_wlan_policy.NetworkConfigChangeError.CREDENTIAL_LEN_ERROR
                )
                client_controller.save_network.return_value = _async_response(
                    res
                )

                with self.assertRaises(HoneydewWlanError):
                    self.wlan_policy_obj.save_network(
                        _TEST_SSID, SecurityType.NONE, None
                    )
                client_controller.save_network.assert_called_once()

            with self.subTest(msg="ZxStatus"):
                res = f_wlan_policy.ClientControllerSaveNetworkResult()
                res.err = (
                    f_wlan_policy.NetworkConfigChangeError.CREDENTIAL_LEN_ERROR
                )
                client_controller.save_network.reset_mock()
                client_controller.save_network.return_value = _async_error(
                    ZxStatus(ZxStatus.ZX_ERR_INTERNAL)
                )

                with self.assertRaises(HoneydewWlanError):
                    self.wlan_policy_obj.save_network(
                        _TEST_SSID, SecurityType.NONE, None
                    )
                client_controller.save_network.assert_called_once()

    def test_scan_for_networks(self) -> None:
        """Test if scan_for_networks works."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            def scan_for_networks(iterator: int) -> None:
                server = TestScanResultIteratorImpl(
                    Channel(iterator),
                    items=[
                        [
                            _TEST_SSID,
                            _TEST_SSID + "2",
                        ],
                        [
                            _TEST_SSID,
                            _TEST_SSID + "3",
                        ],
                    ],
                )
                self.scan_result_iterator = (
                    self.wlan_policy_obj.loop().create_task(server.serve())
                )

            client_controller.scan_for_networks = mock.Mock(
                wraps=scan_for_networks
            )

            networks = self.wlan_policy_obj.scan_for_networks()
            networks.sort()  # order does not matter
            self.assertEqual(
                networks,
                [
                    _TEST_SSID,
                    _TEST_SSID + "2",
                    _TEST_SSID + "3",
                ],
            )

            assert self.scan_result_iterator is not None
            self.wlan_policy_obj.cancel_task(self.scan_result_iterator)

    def test_set_new_update_listener_without_client_controller(self) -> None:
        """Test if set_new_update_listener creates a client controller if it
        doesn't already exist."""
        with self._mock_create_client_controller():
            self.wlan_policy_obj.set_new_update_listener()

            self.assertIsNotNone(self.client_state_updates_proxy)
            self.assertIsNotNone(self.wlan_policy_obj._client_controller)
            assert self.wlan_policy_obj._client_controller is not None
            self.assertEqual(
                self.wlan_policy_obj._client_controller.updates.qsize(), 0
            )

    def test_set_new_update_listener_overrides(self) -> None:
        """Test if set_new_update_listener overrides an existing client state
        updates server."""
        with (
            self._mock_create_client_controller(),
            self._mock_client_listener(),
        ):
            self.wlan_policy_obj.create_client_controller()

            self.assertIsNotNone(self.wlan_policy_obj._client_controller)
            assert self.wlan_policy_obj._client_controller is not None
            old_server = (
                self.wlan_policy_obj._client_controller.client_state_updates_server_task
            )

            self.wlan_policy_obj.set_new_update_listener()

            self.assertIsNotNone(self.wlan_policy_obj._client_controller)
            assert self.wlan_policy_obj._client_controller is not None
            new_server = (
                self.wlan_policy_obj._client_controller.client_state_updates_server_task
            )

            self.assertNotEqual(new_server, old_server)
            self.assertTrue(old_server.cancelled())
            self.assertFalse(new_server.cancelled())

    def test_start_client_connections_passes(self) -> None:
        """Test if start_client_connections passes as expected."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()
            client_controller.start_client_connections.return_value = _async_response(
                f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                    status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                )
            )
            self.wlan_policy_obj.start_client_connections()
            client_controller.start_client_connections.assert_called_once_with()

    def test_start_client_connections_fails(self) -> None:
        """Test if start_client_connections fails in expected ways."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            for msg, resp in [
                (
                    "not supported",
                    _async_response(
                        f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_NOT_SUPPORTED
                        )
                    ),
                ),
                (
                    "incompatible mode",
                    _async_response(
                        f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_INCOMPATIBLE_MODE
                        )
                    ),
                ),
                (
                    "already in use",
                    _async_response(
                        f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_ALREADY_IN_USE
                        )
                    ),
                ),
                (
                    "duplicate request",
                    _async_response(
                        f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_DUPLICATE_REQUEST
                        )
                    ),
                ),
                (
                    "internal error",
                    _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL)),
                ),
            ]:
                with self.subTest(msg=msg, resp=resp):
                    client_controller.start_client_connections.reset_mock()
                    client_controller.start_client_connections.return_value = (
                        resp
                    )
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_policy_obj.start_client_connections()
                    client_controller.start_client_connections.assert_called_once_with()

    def test_start_client_connections_fails_without_client_controller(
        self,
    ) -> None:
        """Test if start_client_connections fails without a client controller."""
        with self.assertRaises(RuntimeError):
            self.wlan_policy_obj.start_client_connections()

    def test_stop_client_connections(self) -> None:
        """Test if stop_client_connections passes as expected."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()
            client_controller.stop_client_connections.return_value = (
                _async_response(
                    f_wlan_policy.ClientControllerStopClientConnectionsResponse(
                        status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                    )
                )
            )
            self.wlan_policy_obj.stop_client_connections()
            client_controller.stop_client_connections.assert_called_once_with()

    def test_stop_client_connections_fails(self) -> None:
        """Test if stop_client_connections fails in expected ways."""
        with self._mock_create_client_controller() as client_controller:
            self.wlan_policy_obj.create_client_controller()

            for msg, resp in [
                (
                    "not supported",
                    _async_response(
                        f_wlan_policy.ClientControllerStopClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_NOT_SUPPORTED
                        )
                    ),
                ),
                (
                    "incompatible mode",
                    _async_response(
                        f_wlan_policy.ClientControllerStopClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_INCOMPATIBLE_MODE
                        )
                    ),
                ),
                (
                    "already in use",
                    _async_response(
                        f_wlan_policy.ClientControllerStopClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_ALREADY_IN_USE
                        )
                    ),
                ),
                (
                    "duplicate request",
                    _async_response(
                        f_wlan_policy.ClientControllerStopClientConnectionsResponse(
                            status=f_wlan_common.RequestStatus.REJECTED_DUPLICATE_REQUEST
                        )
                    ),
                ),
                (
                    "internal error",
                    _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL)),
                ),
            ]:
                with self.subTest(msg=msg, resp=resp):
                    client_controller.stop_client_connections.reset_mock()
                    client_controller.stop_client_connections.return_value = (
                        resp
                    )
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_policy_obj.stop_client_connections()
                    client_controller.stop_client_connections.assert_called_once_with()

    def test_stop_client_connections_fails_without_client_controller(
        self,
    ) -> None:
        """Test if stop_client_connections fails without a client controller."""
        with self.assertRaises(RuntimeError):
            self.wlan_policy_obj.stop_client_connections()


class TestScanResultIteratorImpl(f_wlan_policy.ScanResultIterator.Server):
    """Iterator for scan results."""

    def __init__(self, server: Channel, items: list[list[str]]) -> None:
        super().__init__(server)
        self._items = items

    def get_next(self) -> f_wlan_policy.ScanResultIteratorGetNextResponse:
        """Get next set of scan result SSIDs."""
        if len(self._items) == 0:
            raise ZxStatus(ZxStatus.ZX_ERR_PEER_CLOSED)
        return f_wlan_policy.ScanResultIteratorGetNextResponse(
            scan_results=[
                _make_scan_result(ssid) for ssid in self._items.pop(0)
            ],
        )


class TestNetworkConfigIteratorImpl(f_wlan_policy.NetworkConfigIterator.Server):
    """Iterator for NetworkConfig results."""

    def __init__(
        self, server: Channel, items: list[list[f_wlan_policy.NetworkConfig]]
    ) -> None:
        super().__init__(server)
        self._items = items

    def get_next(
        self,
    ) -> f_wlan_policy.NetworkConfigIteratorGetNextResponse:
        """Get next set of NetworkConfigs."""
        if len(self._items) == 0:
            raise ZxStatus(ZxStatus.ZX_ERR_PEER_CLOSED)
        return f_wlan_policy.NetworkConfigIteratorGetNextResponse(
            configs=self._items.pop(0),
        )


if __name__ == "__main__":
    unittest.main()
