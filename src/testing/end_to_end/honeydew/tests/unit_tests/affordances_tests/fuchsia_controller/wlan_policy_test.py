# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy."""

import unittest
from collections.abc import Iterator
from contextlib import contextmanager
from typing import TypeVar
from unittest import mock

import fidl.fuchsia_wlan_common as f_wlan_common
import fidl.fuchsia_wlan_policy as f_wlan_policy
from fuchsia_controller_py import Channel, ZxStatus

from honeydew.affordances.fuchsia_controller.wlan import wlan_policy
from honeydew.errors import HoneydewWlanError, NotSupportedError
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import fuchsia_controller as fc_transport
from honeydew.typing.wlan import (
    ClientStateSummary,
    SecurityType,
    WlanClientState,
)

_T = TypeVar("_T")


async def _async_response(response: _T) -> _T:
    return response


async def _async_error(err: Exception) -> None:
    raise err


# pylint: disable=protected-access
class WlanPolicyFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.wlan.wlan_policy."""

    def setUp(self) -> None:
        super().setUp()

        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice,
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
            wlan_policy._REQUIRED_CAPABILITIES
        )

        self.wlan_policy_obj = wlan_policy.WlanPolicy(
            device_name="fuchsia-emulator",
            ffx=self.ffx_transport_obj,
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )
        self.client_state_updates_proxy: (
            f_wlan_policy.ClientStateUpdatesClient | None
        ) = None

    def tearDown(self) -> None:
        self.wlan_policy_obj.close()
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

    def test_verify_supported(self) -> None:
        """Test if _verify_supported works."""
        self.ffx_transport_obj.run.return_value = ""

        with self.assertRaises(NotSupportedError):
            self.wlan_policy_obj = wlan_policy.WlanPolicy(
                device_name="fuchsia-emulator",
                ffx=self.ffx_transport_obj,
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )

    def test_init_connect_proxy(self) -> None:
        """Test if WlanPolicy connects to WLAN Policy proxies."""
        self.assertIsNotNone(self.wlan_policy_obj._client_provider_proxy)

    def test_connect(self) -> None:
        """Test if connect works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.connect("", SecurityType.NONE)

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

            update = (
                self.wlan_policy_obj._async_adapter_loop.run_until_complete(
                    self.wlan_policy_obj._client_controller.updates.get()
                )
            )
            self.assertEqual(
                update,
                ClientStateSummary(
                    state=WlanClientState.CONNECTIONS_ENABLED, networks=[]
                ),
            )

    def test_get_saved_networks(self) -> None:
        """Test if get_saved_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_saved_networks()

    def test_get_update(self) -> None:
        """Test if get_update works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.get_update()

    def test_remove_all_networks(self) -> None:
        """Test if remove_all_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.remove_all_networks()

    def test_remove_network(self) -> None:
        """Test if remove_network works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.remove_network("", SecurityType.NONE)

    def test_save_network(self) -> None:
        """Test if save_network works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.save_network("", SecurityType.NONE)

    def test_scan_for_networks(self) -> None:
        """Test if scan_for_networks works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.scan_for_networks()

    def test_set_new_update_listener(self) -> None:
        """Test if set_new_update_listener works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.set_new_update_listener()

    def test_start_client_connections_passes(self) -> None:
        """Test if start_client_connections passes as expected."""
        with self._mock_create_client_controller() as client_controller:
            client_controller.start_client_connections.return_value = _async_response(
                f_wlan_policy.ClientControllerStartClientConnectionsResponse(
                    status=f_wlan_common.RequestStatus.ACKNOWLEDGED
                )
            )
            self.wlan_policy_obj.start_client_connections()

    def test_start_client_connections_fails(self) -> None:
        """Test if start_client_connections fails in expected ways."""
        with self._mock_create_client_controller() as client_controller:
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
                    client_controller.start_client_connections.return_value = (
                        resp
                    )
                    with self.assertRaises(HoneydewWlanError):
                        self.wlan_policy_obj.start_client_connections()

    def test_stop_client_connections(self) -> None:
        """Test if stop_client_connections works."""
        with self.assertRaises(NotImplementedError):
            self.wlan_policy_obj.stop_client_connections()


if __name__ == "__main__":
    unittest.main()
