# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
# pylint: disable=protected-access
"""Unit tests for bluetooth_common_using_fc.py"""

import asyncio
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fidl.fuchsia_bluetooth as f_bt
import fidl.fuchsia_bluetooth_sys as f_btsys_controller
from parameterized import param, parameterized

from honeydew.affordances.connectivity.bluetooth.bluetooth_common import (
    bluetooth_common_using_fc,
)
from honeydew.affordances.connectivity.bluetooth.utils import (
    errors as bluetooth_errors,
)
from honeydew.affordances.connectivity.bluetooth.utils import (
    types as bluetooth_types,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport

BluetoothAcceptPairing = bluetooth_types.BluetoothAcceptPairing
BluetoothConnectionType = bluetooth_types.BluetoothConnectionType

_SAMPLE_KNOWN_DEVICES_OUTPUT = f_btsys_controller.AccessWatchPeersResponse(
    updated=[
        f_btsys_controller.Peer(
            id=f_bt.PeerId(value=16085008211800713200),
            address=f_bt.Address(
                bytes=[88, 111, 107, 249, 15, 248], type=f_bt.AddressType(1)
            ),
            technology=f_btsys_controller.TechnologyType(2),
            connected=True,
            bonded=True,
            name=f_bt.DeviceName("fuchsia-f80f-f96b-6f59"),
            rssi=17,
        ),
    ],
    removed=[
        f_bt.PeerId(value="0"),
    ],
)

_ACTUAL_KNOWN_DEVICE_OUTPUT: f_btsys_controller.AccessWatchPeersResponse = {
    "16085008211800713200": {
        "address": [88, 111, 107, 249, 15, 248],
        "appearance": None,
        "bonded": True,
        "connected": True,
        "id": 16085008211800713200,
        "name": "fuchsia-f80f-f96b-6f59",
        "rssi": 17,
        "services": None,
        "technology": 2,
        "tx_power": None,
    }
}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


class BluetoothCommonFCTests(unittest.TestCase):
    """Unit tests for bluetooth_common_using_fc.py."""

    def setUp(self) -> None:
        super().setUp()
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController
        )

        self.bluetooth_common_fc_obj = (
            bluetooth_common_using_fc.BluetoothCommonUsingFc(
                device_name="fuchsia-emulator",
                fuchsia_controller=self.fc_transport_obj,
                reboot_affordance=self.reboot_affordance_obj,
            )
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "when_session_not_initialized",
                    "session_initialized": False,
                },
            ),
            (
                {
                    "label": "when_session_already_initialized",
                    "session_initialized": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_sys_init(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for BluetoothGap.sys_init() method."""
        # Check whether an `BluetoothError` exception is raised when
        # calling `sys_init()` on a session that is already initialized.
        if parameterized_dict.get("session_initialized"):
            with self.assertRaises(bluetooth_errors.BluetoothStateError):
                self.bluetooth_common_fc_obj.sys_init()
        else:
            assert (
                self.bluetooth_common_fc_obj._access_controller_proxy
                is not None
            )
            assert (
                self.bluetooth_common_fc_obj._host_watcher_controller_proxy
                is not None
            )
            assert (
                self.bluetooth_common_fc_obj._pairing_controller_proxy
                is not None
            )
            assert not self.bluetooth_common_fc_obj.known_devices

    def test_reset_state(self) -> None:
        """Test for BluetoothGap.reset_state() method."""
        self.bluetooth_common_fc_obj._pairing_delegate_server = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.reset_state()
        assert self.bluetooth_common_fc_obj._access_controller_proxy is None
        assert (
            self.bluetooth_common_fc_obj._host_watcher_controller_proxy is None
        )
        assert self.bluetooth_common_fc_obj._pairing_controller_proxy is None
        assert self.bluetooth_common_fc_obj._session_initialized is False
        assert self.bluetooth_common_fc_obj._pairing_delegate_server is None
        self.bluetooth_common_fc_obj.loop.close.assert_called()

    def test_accept_pairing(self) -> None:
        """Test for BluetoothGap.accept_pairing() method."""
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.accept_pairing(
            BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
            BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE,
        )
        self.bluetooth_common_fc_obj.loop.run_until_complete.assert_called()

    def test_async_accept_pairing(self) -> None:
        """Test for BluetoothGap._accept_pairing() async method."""
        self.bluetooth_common_fc_obj._pairing_controller_proxy = (
            mock.MagicMock()
        )
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        asyncio.run(
            self.bluetooth_common_fc_obj._accept_pairing(
                BluetoothAcceptPairing.DEFAULT_INPUT_MODE,
                BluetoothAcceptPairing.DEFAULT_OUTPUT_MODE,
            )
        )
        assert self.bluetooth_common_fc_obj._pairing_delegate_server is not None
        self.bluetooth_common_fc_obj._pairing_controller_proxy.set_pairing_delegate.assert_called_with(
            input=1, output=1, delegate=mock.ANY
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "pair_classic",
                    "transport": BluetoothConnectionType.CLASSIC,
                },
            ),
            (
                {
                    "label": "pair_low_energy",
                    "transport": BluetoothConnectionType.LOW_ENERGY,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_connect_device(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for BluetoothGap.connect_device() method."""
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        dummy_identifier = "0"
        self.bluetooth_common_fc_obj.connect_device(
            identifier=dummy_identifier,
            connection_type=parameterized_dict["transport"],
        )
        dummy_peer_id = f_bt.PeerId(value=dummy_identifier)
        self.bluetooth_common_fc_obj._access_controller_proxy.connect.assert_called_with(
            id=dummy_peer_id
        )
        self.assertEqual(
            self.bluetooth_common_fc_obj.loop.run_until_complete.call_count, 2
        )

    def test_forget_device(self) -> None:
        """Test for BluetoothGap.forget_device() method."""
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        dummy_identifier = "0"
        self.bluetooth_common_fc_obj.forget_device(
            identifier=dummy_identifier,
        )
        dummy_peer_id = f_bt.PeerId(value=dummy_identifier)
        self.bluetooth_common_fc_obj._access_controller_proxy.forget.assert_called_with(
            id=dummy_peer_id
        )
        self.bluetooth_common_fc_obj.loop.run_until_complete.assert_called_once()

    def test_get_active_adapter_address(self) -> None:
        """Test for BluetoothGap.get_active_adapter_address() method."""
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop.run_until_complete = mock.MagicMock(
            return_value="1"
        )
        dummy_address = (
            self.bluetooth_common_fc_obj.get_active_adapter_address()
        )
        self.assertEqual(dummy_address, "1")

    @unittest.skip("Skipping due to unresolved TypeError")
    def test_async_get_active_adapter_address(self) -> None:
        """Test for BluetoothGap.get_active_adapter_address() async method."""
        self.bluetooth_common_fc_obj._host_watcher_controller_proxy = (
            mock.MagicMock()
        )
        test = f_btsys_controller.HostWatcherWatchResponse(
            hosts=[
                f_btsys_controller.HostInfo(
                    addresses=[
                        f_bt.Address(
                            bytes=[88, 111, 107, 249, 15, 248], type="0"
                        )
                    ]
                )
            ]
        )
        self.bluetooth_common_fc_obj._host_watcher_controller_proxy.watch = (
            mock.MagicMock(return_value=test)
        )
        res = asyncio.run(self.bluetooth_common_fc_obj._get_active_address())
        self.assertEqual(res, [88, 111, 107, 249, 15, 248])

    def test_get_known_remote_devices(self) -> None:
        """Test for BluetoothGap.get_known_remote_devices() method."""
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop.run_until_complete = mock.MagicMock(
            return_value=_SAMPLE_KNOWN_DEVICES_OUTPUT
        )
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        data = self.bluetooth_common_fc_obj.get_known_remote_devices()
        self.assertEqual(data, _ACTUAL_KNOWN_DEVICE_OUTPUT)

    def test_get_connected_devices(self) -> None:
        """Test for BluetoothGap.get_connected_devices() method."""
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop.run_until_complete = mock.MagicMock(
            return_value=_SAMPLE_KNOWN_DEVICES_OUTPUT
        )
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        data = self.bluetooth_common_fc_obj.get_connected_devices()
        self.assertEqual(data, [16085008211800713200])

    @parameterized.expand(
        [
            (
                {
                    "label": "pair_classic",
                    "transport": BluetoothConnectionType.CLASSIC,
                },
            ),
            (
                {
                    "label": "pair_low_energy",
                    "transport": BluetoothConnectionType.LOW_ENERGY,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_pair_device(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for BluetoothGap.pair_device() method."""
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        dummy_identifier = "0"
        self.bluetooth_common_fc_obj.pair_device(
            identifier=dummy_identifier,
            connection_type=parameterized_dict["transport"],
        )
        dummy_peer_id = f_bt.PeerId(value=dummy_identifier)
        dummy_options = f_btsys_controller.PairingOptions(
            le_security_level=None, bondable_mode=None, transport=None
        )
        self.bluetooth_common_fc_obj._access_controller_proxy.pair.assert_called_with(
            id=dummy_peer_id, options=dummy_options
        )
        self.assertEqual(
            self.bluetooth_common_fc_obj.loop.run_until_complete.call_count, 2
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "discovery_true_with_token",
                    "discovery": True,
                    "token": True,
                },
            ),
            (
                {
                    "label": "discovery_true_without_token",
                    "discovery": True,
                    "token": False,
                },
            ),
            (
                {
                    "label": "discovery_false_without_token",
                    "discovery": False,
                    "token": False,
                },
            ),
            (
                {
                    "label": "discovery_false_with_token",
                    "discovery": False,
                    "token": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_request_discovery(
        self, parameterized_dict: dict[str, Any]
    ) -> None:
        """Test for BluetoothGap.request_discovery() method."""
        if parameterized_dict.get("token"):
            self.bluetooth_common_fc_obj.discovery_token = mock.MagicMock()
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        if parameterized_dict.get("token") and parameterized_dict.get(
            "discovery"
        ):
            with self.assertRaises(bluetooth_errors.BluetoothError):
                self.bluetooth_common_fc_obj.request_discovery(discovery=True)
        elif parameterized_dict.get("discovery"):
            self.bluetooth_common_fc_obj.request_discovery(discovery=True)
            self.bluetooth_common_fc_obj._access_controller_proxy.start_discovery.assert_called_once()
            self.assertEqual(
                self.bluetooth_common_fc_obj.loop.run_until_complete.call_count,
                1,
            )
        else:
            self.bluetooth_common_fc_obj.request_discovery(discovery=False)
            assert self.bluetooth_common_fc_obj.discovery_token is None

    @parameterized.expand(
        [
            (
                {
                    "label": "discoverable_true_with_token",
                    "discoverable": True,
                    "token": True,
                },
            ),
            (
                {
                    "label": "discoverable_true_without_token",
                    "discoverable": True,
                    "token": False,
                },
            ),
            (
                {
                    "label": "discoverable_false_without_token",
                    "discoverable": False,
                    "token": False,
                },
            ),
            (
                {
                    "label": "discoverable_false_with_token",
                    "discoverable": False,
                    "token": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_set_discoverable(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for BluetoothGap.set_discoverable() method."""
        if parameterized_dict.get("token"):
            self.bluetooth_common_fc_obj.discoverable_token = mock.MagicMock()
        self.bluetooth_common_fc_obj._access_controller_proxy = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        if not parameterized_dict.get("discoverable"):
            self.bluetooth_common_fc_obj.set_discoverable(discoverable=False)
            assert self.bluetooth_common_fc_obj.discoverable_token is None
            return
        if parameterized_dict.get("token"):
            with self.assertRaises(bluetooth_errors.BluetoothError):
                self.bluetooth_common_fc_obj.set_discoverable(discoverable=True)
        else:
            self.bluetooth_common_fc_obj.set_discoverable(discoverable=True)
            self.bluetooth_common_fc_obj._access_controller_proxy.make_discoverable.assert_called_once()
            self.assertEqual(
                self.bluetooth_common_fc_obj.loop.run_until_complete.call_count,
                1,
            )

    @parameterized.expand(
        [
            (
                {
                    "label": "with_active_pairing_delegate",
                    "server": True,
                },
            ),
            (
                {
                    "label": "without_active_pairing_delegate",
                    "server": False,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_run_pairing_delegate(
        self, parameterized_dict: dict[str, Any]
    ) -> None:
        """Test for BluetoothGap.run_pairing_delegate()."""
        if not parameterized_dict.get("server"):
            with self.assertRaises(bluetooth_errors.BluetoothError):
                self.bluetooth_common_fc_obj.run_pairing_delegate()
        self.bluetooth_common_fc_obj._pairing_delegate_server = mock.MagicMock()
        self.bluetooth_common_fc_obj.loop = mock.MagicMock()
        self.bluetooth_common_fc_obj.run_pairing_delegate()
        self.assertEqual(
            self.bluetooth_common_fc_obj.loop.run_until_complete.call_count, 1
        )


if __name__ == "__main__":
    unittest.main()
