# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for wlan_policy_ap_using_fc.py"""

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
    AccessPointState,
    ConnectivityMode,
    NetworkIdentifier,
    OperatingBand,
    OperatingState,
    SecurityType,
)
from honeydew.affordances.connectivity.wlan.wlan_policy_ap import (
    wlan_policy_ap_using_fc,
)
from honeydew.errors import NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport

_TEST_SSID = "ThepromisedLAN"
_TEST_SSID_BYTES = list(str.encode(_TEST_SSID))

_ACCESS_POINT_STATE = AccessPointState(
    state=OperatingState.STARTING,
    mode=ConnectivityMode.LOCAL_ONLY,
    band=OperatingBand.ONLY_2_4GHZ,
    frequency=None,
    clients=None,
    id=NetworkIdentifier(ssid=_TEST_SSID, security_type=SecurityType.WPA2),
)
_ACCESS_POINT_STATE_FIDL = f_wlan_policy.AccessPointState(
    state=f_wlan_policy.OperatingState.STARTING,
    mode=f_wlan_policy.ConnectivityMode.LOCAL_ONLY,
    band=f_wlan_policy.OperatingBand.ONLY_2_4GHZ,
    frequency=None,
    clients=None,
    id=f_wlan_policy.NetworkIdentifier(
        ssid=list(_TEST_SSID_BYTES),
        type=f_wlan_policy.SecurityType.WPA2,
    ),
)


_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


async def _async_error(err: Exception) -> None:
    raise err


# pylint: disable=protected-access
class WlanPolicyApFCTests(unittest.TestCase):
    """Unit tests for wlan_policy_ap_using_fc.py"""

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
            wlan_policy_ap_using_fc._REQUIRED_CAPABILITIES
        )

        self.access_point_state_updates_proxy: (
            f_wlan_policy.AccessPointStateUpdatesClient | None
        ) = None

        with (
            self.mock_ap_provider(),
            self.mock_ap_listener(),
            self.mock_ap_controller() as ap_controller_client,
        ):
            self.access_point_controller_obj = ap_controller_client

            self.wlan_policy_ap_obj = wlan_policy_ap_using_fc.WlanPolicyAp(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
                fuchsia_device_close=self.fuchsia_device_close_obj,
            )

        self.assertFalse(
            self.wlan_policy_ap_obj._access_point_controller.access_point_state_updates_server_task.cancelled(),
            "Expected access point state update server to be running",
        )

        self.assertIsNotNone(self.access_point_state_updates_proxy)
        assert self.access_point_state_updates_proxy is not None

    def tearDown(self) -> None:
        self.wlan_policy_ap_obj._close()
        return super().tearDown()

    @contextmanager
    def mock_ap_provider(self) -> Iterator[mock.MagicMock]:
        """Mock requests to fuchsia.wlan.policy/AccessPointProvider."""
        ap_provider_client = mock.MagicMock(
            spec=f_wlan_policy.AccessPointProvider.Client,
            autospec=True,
        )

        # pylint: disable-next=unused-argument
        def get_controller(requests: Channel, updates: Channel) -> None:
            self.access_point_state_updates_proxy = (
                f_wlan_policy.AccessPointStateUpdates.Client(updates)
            )

        ap_provider_client.get_controller = mock.Mock(wraps=get_controller)

        with mock.patch(
            "fidl.fuchsia_wlan_policy.AccessPointProvider", autospec=True
        ) as fidl_mock:
            fidl_mock.Client.return_value = ap_provider_client
            yield ap_provider_client

    @contextmanager
    def mock_ap_listener(self) -> Iterator[mock.MagicMock]:
        """Mock requests to fuchsia.wlan.policy/AccessPointListener."""
        ap_listener_client = mock.MagicMock(
            spec=f_wlan_policy.AccessPointListener.Client,
            autospec=True,
        )

        def get_listener(updates: Channel) -> None:
            self.access_point_state_updates_proxy = (
                f_wlan_policy.AccessPointStateUpdates.Client(updates)
            )

        ap_listener_client.get_listener = mock.Mock(wraps=get_listener)

        with mock.patch(
            "fidl.fuchsia_wlan_policy.AccessPointListener", autospec=True
        ) as fidl_mock:
            fidl_mock.Client.return_value = ap_listener_client
            yield ap_listener_client

    @contextmanager
    def mock_ap_controller(self) -> Iterator[mock.MagicMock]:
        ap_controller_client = mock.MagicMock(
            spec=f_wlan_policy.AccessPointController.Client,
            autospec=True,
        )

        with mock.patch(
            "fidl.fuchsia_wlan_policy.AccessPointController", autospec=True
        ) as fidl_mock:
            fidl_mock.Client.return_value = ap_controller_client
            yield ap_controller_client

    def test_verify_supported(self) -> None:
        """Verify verify_supported fails."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.wlan_policy_ap_obj = wlan_policy_ap_using_fc.WlanPolicyAp(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
                fuchsia_device_close=self.fuchsia_device_close_obj,
            )

    def test_init_register_for_on_device_boot(self) -> None:
        """Verify WlanPolicyAp registers on_device_boot."""
        self.reboot_affordance_obj.register_for_on_device_boot.assert_called_once_with(
            self.wlan_policy_ap_obj._connect_proxy
        )

    def test_init_connect_proxy(self) -> None:
        """Verify WlanPolicyAp connects to
        fuchsia.wlan.policy/AccessPointProvider and AccessPointListener."""
        self.assertIsNotNone(
            self.wlan_policy_ap_obj._access_point_provider_proxy
        )
        self.assertIsNotNone(
            self.wlan_policy_ap_obj._access_point_listener_proxy
        )

    def test_start(self) -> None:
        """Verify WlanPolicyAp.start()."""
        self.access_point_controller_obj.start_access_point.side_effect = [
            _async_response(
                f_wlan_policy.AccessPointControllerStartAccessPointResponse(
                    status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                )
            )
        ]

        self.wlan_policy_ap_obj.start(
            _TEST_SSID,
            SecurityType.NONE,
            None,
            ConnectivityMode.LOCAL_ONLY,
            OperatingBand.ANY,
        )

    def test_start_fails(self) -> None:
        """Verify WlanPolicyAp.start() throws HoneydewWlanError on internal error."""
        self.access_point_controller_obj.start_access_point.side_effect = [
            _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL))
        ]

        with self.assertRaises(HoneydewWlanError):
            self.wlan_policy_ap_obj.start(
                _TEST_SSID,
                SecurityType.NONE,
                None,
                ConnectivityMode.LOCAL_ONLY,
                OperatingBand.ANY,
            )

    def test_stop(self) -> None:
        """Verify WlanPolicyAp.stop()."""
        self.access_point_controller_obj.stop_access_point.side_effect = [
            _async_response(
                f_wlan_policy.AccessPointControllerStartAccessPointResponse(
                    status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                )
            )
        ]

        self.wlan_policy_ap_obj.stop(
            _TEST_SSID,
            SecurityType.NONE,
            None,
        )

    def test_stop_fails(self) -> None:
        """Verify WlanPolicyAp.stop() throws HoneydewWlanError on internal error."""
        self.access_point_controller_obj.stop_access_point.side_effect = [
            _async_error(ZxStatus(ZxStatus.ZX_ERR_INTERNAL))
        ]

        with self.assertRaises(HoneydewWlanError):
            self.wlan_policy_ap_obj.stop(
                _TEST_SSID,
                SecurityType.NONE,
                None,
            )

    def test_stop_all(self) -> None:
        """Verify WlanPolicyAp.stop_all()."""
        self.wlan_policy_ap_obj.stop_all()

    def test_set_new_update_listener_overrides(self) -> None:
        """Verify WlanPolicyAp.set_new_update_listener() overrides the existing
        access point state updates server."""
        old_server = (
            self.wlan_policy_ap_obj._access_point_controller.access_point_state_updates_server_task
        )

        with self.mock_ap_listener():
            self.wlan_policy_ap_obj.set_new_update_listener()

        new_server = (
            self.wlan_policy_ap_obj._access_point_controller.access_point_state_updates_server_task
        )

        self.assertNotEqual(new_server, old_server)
        self.assertTrue(old_server.cancelled())
        self.assertFalse(new_server.cancelled())
        self.assertEqual(
            self.wlan_policy_ap_obj._access_point_controller.updates.qsize(),
            0,
        )

    def test_get_update(self) -> None:
        """Verify WlanPolicyAp.get_update()."""
        self.assertIsNotNone(self.access_point_state_updates_proxy)
        assert self.access_point_state_updates_proxy is not None

        self.wlan_policy_ap_obj.loop().run_until_complete(
            self.access_point_state_updates_proxy.on_access_point_state_update(
                access_points=[
                    _ACCESS_POINT_STATE_FIDL,
                ]
            )
        )
        self.assertEqual(
            self.wlan_policy_ap_obj.get_update(), [_ACCESS_POINT_STATE]
        )

    def test_get_update_queuing(self) -> None:
        """Verify WlanPolicyAp.get_update() queues updates."""
        self.assertIsNotNone(self.access_point_state_updates_proxy)
        assert self.access_point_state_updates_proxy is not None

        self.wlan_policy_ap_obj.loop().run_until_complete(
            self.access_point_state_updates_proxy.on_access_point_state_update(
                access_points=[]
            )
        )
        self.wlan_policy_ap_obj.loop().run_until_complete(
            self.access_point_state_updates_proxy.on_access_point_state_update(
                access_points=[_ACCESS_POINT_STATE_FIDL]
            )
        )
        self.assertEqual(self.wlan_policy_ap_obj.get_update(), [])
        self.assertEqual(
            self.wlan_policy_ap_obj.get_update(), [_ACCESS_POINT_STATE]
        )

    @mock.patch(
        "asyncio.wait_for", autospec=True, side_effect=[asyncio.TimeoutError]
    )
    def test_get_update_timeout(self, wait_for_mock: mock.MagicMock) -> None:
        """Verify WlanPolicyAp.get_update() throws TimeoutError on timeout."""
        with self.assertRaises(TimeoutError):
            self.wlan_policy_ap_obj.get_update(timeout=10)
        wait_for_mock.assert_called_once()


if __name__ == "__main__":
    unittest.main()
