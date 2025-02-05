#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
# pylint: disable=protected-access
"""Unit tests for honeydew.affordances.fuchsia_controller.bluetooth.profiles.bluetooth_le.py"""

import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fidl.fuchsia_bluetooth as f_bt
import fidl.fuchsia_bluetooth_gatt2 as f_gatt_controller
import fidl.fuchsia_bluetooth_le as f_ble_controller
from parameterized import param, parameterized

from honeydew.affordances.connectivity.bluetooth.le import le_using_fc
from honeydew.affordances.connectivity.bluetooth.utils import (
    errors as bluetooth_errors,
)
from honeydew.affordances.connectivity.bluetooth.utils import types as bt_types
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import fuchsia_controller as fc_transport

_SAMPLE_LE_KNOWN_DEVICES_OUTPUT: dict[str, Any] = {
    "16085008211800713200": {
        "bonded": True,
        "connectable": True,
        "id": 16085008211800713200,
        "name": "fuchsia-f80f-f96b-6f59",
    }
}

_SAMPLE_CLIENT_WATCH_SERVICES_RESPONSE = (
    f_gatt_controller.ClientWatchServicesResponse(
        updated=[
            f_gatt_controller.ServiceInfo(
                handle=f_gatt_controller.ServiceHandle(value=164),
                kind=1,
                type=f_bt.Uuid(value=[1]),
                characteristics=None,
                includes=None,
            )
        ],
        removed=[],
    )
)

_SAMPLE_GATT_SERVICES_OUTPUT: dict[int, dict[str, Any]] = {
    164: {
        "kind": 1,
        "type": f_bt.Uuid(value=[1]),
        "characteristics": None,
        "includes": None,
    }
}

_SAMPLE_DISCOVER_CHARACTERISTIC_RESPONSE = (
    f_gatt_controller.RemoteServiceDiscoverCharacteristicsResponse(
        characteristics=[
            f_gatt_controller.Characteristic(
                handle=f_gatt_controller.Handle(value=22),
                type=f_bt.Uuid(value=[1]),
                properties=2,
                permissions=None,
                descriptors=None,
            )
        ]
    )
)

_SAMPLE_DISCOVER_CHARACTERISTIC_OUTPUT: dict[
    int, dict[str, int | list[int] | None]
] = {
    22: {
        "handle": 22,
        "type": [1],
        "properties": 2,
        "permissions": None,
        "descriptors": None,
    }
}

_SAMPLE_READ_CHARACTERISTIC_OUTPUT: dict[str, int | list[int] | None | bool] = {
    "handle": 22,
    "value": [1],
    "truncated": False,
}

_SAMPLE_READ_CHARACTERISTIC_RESPONSE = (
    f_gatt_controller.RemoteServiceReadCharacteristicResponse(
        value=f_gatt_controller.ReadValue(
            handle=f_gatt_controller.Handle(value=22),
            value=[1],
            maybe_truncated=False,
        )
    )
)


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_{test_label}"


class BluetoothLETest(unittest.TestCase):
    def setUp(self) -> None:
        super().setUp()
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )
        self.fc_transport_obj = mock.MagicMock(
            spec=fc_transport.FuchsiaController
        )

        self.bluetooth_le_obj = le_using_fc.LEUsingFc(
            device_name="fuchsia-emulator",
            fuchsia_controller=self.fc_transport_obj,
            reboot_affordance=self.reboot_affordance_obj,
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
    def test_init_le_sys(self, parameterized_dict: dict[str, Any]) -> None:
        """Test for BluetoothLE.sys_init() method."""
        # Check whether an `BluetoothError` exception is raised when
        # calling `sys_init()` on a session that is already initialized.
        if parameterized_dict.get("session_initialized"):
            with self.assertRaises(bluetooth_errors.BluetoothStateError):
                self.bluetooth_le_obj.sys_init()
        else:
            assert (
                self.bluetooth_le_obj._peripheral_controller_proxy is not None
            )
            assert self.bluetooth_le_obj._central_controller_proxy is not None
            assert self.bluetooth_le_obj._gatt_server_proxy is not None
            assert not self.bluetooth_le_obj.known_le_devices
            assert isinstance(self.bluetooth_le_obj._uuid, f_bt.Uuid)

    def test_reset_state(self) -> None:
        """Test for BluetoothLE.reset_state() method."""
        self.bluetooth_le_obj._peripheral_advertisement_server = (
            mock.MagicMock()
        )
        self.bluetooth_le_obj.loop = mock.MagicMock()
        self.bluetooth_le_obj.reset_state()
        assert self.bluetooth_le_obj._peripheral_controller_proxy is None
        assert self.bluetooth_le_obj._central_controller_proxy is None
        assert self.bluetooth_le_obj._gatt_server_proxy is None
        assert self.bluetooth_le_obj._le_session_initialized is False
        assert self.bluetooth_le_obj._peripheral_advertisement_server is None
        assert self.bluetooth_le_obj._peripheral_connection is None

    def test_stop_advertise(self) -> None:
        """test for BluetoothLE.stop_advertise() method."""
        self.bluetooth_le_obj._peripheral_advertisement_server = (
            mock.MagicMock()
        )
        self.bluetooth_le_obj.stop_advertise()
        assert self.bluetooth_le_obj._peripheral_advertisement_server is None

    def test_scan(self) -> None:
        """test for BluetoothLE.scan() method."""
        self.bluetooth_le_obj.loop = mock.MagicMock()
        self.bluetooth_le_obj.loop.run_until_complete = mock.MagicMock(
            return_value=_SAMPLE_LE_KNOWN_DEVICES_OUTPUT
        )
        self.bluetooth_le_obj._central_controller_proxy = mock.MagicMock()
        data = self.bluetooth_le_obj.scan()
        self.assertEqual(data, _SAMPLE_LE_KNOWN_DEVICES_OUTPUT)

    def test_connect(self) -> None:
        """test for BluetoothLE.connect() method."""
        self.bluetooth_le_obj._central_controller_proxy = mock.MagicMock()
        self.bluetooth_le_obj.loop = mock.MagicMock()
        mock_identifier = 0
        self.bluetooth_le_obj.connect(identifier=mock_identifier)
        mock_peer_id = f_bt.PeerId(value=mock_identifier)
        mock_options = f_ble_controller.ConnectionOptions(bondable_mode=True)
        self.bluetooth_le_obj._central_controller_proxy.connect.assert_called_with(
            id=mock_peer_id, options=mock_options, handle=mock.ANY
        )
        self.assertEqual(
            self.bluetooth_le_obj.loop.run_until_complete.call_count, 1
        )

    def test_advertise(self) -> None:
        """test for BluetoothLE.advertise() method."""
        self.bluetooth_le_obj.loop = mock.MagicMock()
        mock_appearance = bt_types.BluetoothLEAppearance.GLUCOSE_MONITOR
        mock_name = "mock_name"
        self.bluetooth_le_obj.advertise(
            appearance=mock_appearance, name=mock_name
        )
        self.assertEqual(
            self.bluetooth_le_obj.loop.run_until_complete.call_count, 1
        )

    def test_async_advertise(self) -> None:
        """test for BluetoothLE.advertise() async method."""
        mock_appearance = bt_types.BluetoothLEAppearance.GLUCOSE_MONITOR
        mock_name = "mock_name"
        mock_uuid = self.bluetooth_le_obj._uuid
        mock_connections = f_ble_controller.ConnectionOptions(
            bondable_mode=True
        )
        mock_advertising_data = f_ble_controller.AdvertisingData(
            name=mock_name,
            appearance=mock_appearance,
            service_uuids=[mock_uuid],
        )
        mock_params = f_ble_controller.AdvertisingParameters(
            data=mock_advertising_data, connection_options=mock_connections
        )
        self.bluetooth_le_obj._peripheral_advertisement_server = (
            mock.MagicMock()
        )
        self.bluetooth_le_obj._peripheral_controller_proxy = mock.MagicMock()
        self.bluetooth_le_obj.advertise(
            appearance=mock_appearance, name=mock_name
        )
        self.bluetooth_le_obj._peripheral_controller_proxy.advertise.assert_called_with(
            parameters=mock_params, advertised_peripheral=mock.ANY
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "with_active_peripheral_server",
                    "server": True,
                },
            ),
            (
                {
                    "label": "without_active_peripheral_server",
                    "server": False,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_run_advertise_connection(
        self, parameterized_dict: dict[str, Any]
    ) -> None:
        """Test for BluetoothLE.run_advertise_connection()."""
        if not parameterized_dict.get("server"):
            with self.assertRaises(bluetooth_errors.BluetoothError):
                self.bluetooth_le_obj.run_advertise_connection()
        self.bluetooth_le_obj._peripheral_advertisement_server = (
            mock.MagicMock()
        )
        self.bluetooth_le_obj.loop = mock.MagicMock()
        self.bluetooth_le_obj.run_advertise_connection()
        self.assertEqual(
            self.bluetooth_le_obj.loop.run_until_complete.call_count, 1
        )

    def test_request_gatt_client(self) -> None:
        """Test for BluetoothLE.request_gatt_client()."""
        self.bluetooth_le_obj._connection_client = mock.MagicMock()
        self.bluetooth_le_obj.request_gatt_client()
        self.bluetooth_le_obj._connection_client.request_gatt_client.assert_called_with(
            client=mock.ANY
        )
        assert self.bluetooth_le_obj._gatt_client is not None

    def test_list_gatt_services(self) -> None:
        """Test for BluetoothLE.list_gatt_services()."""
        self.bluetooth_le_obj._gatt_client = mock.MagicMock()
        self.bluetooth_le_obj.loop = mock.MagicMock()
        self.bluetooth_le_obj.loop.run_until_complete = mock.MagicMock(
            return_value=_SAMPLE_CLIENT_WATCH_SERVICES_RESPONSE
        )
        data = self.bluetooth_le_obj.list_gatt_services()
        self.assertEqual(data, _SAMPLE_GATT_SERVICES_OUTPUT)

    def test_connect_to_service(self) -> None:
        """Test for BluetoothLE.connect_to_service()."""
        mock_handle = 1
        self.bluetooth_le_obj._gatt_client = mock.MagicMock()
        self.bluetooth_le_obj.connect_to_service(handle=mock_handle)
        self.bluetooth_le_obj._gatt_client.connect_to_service.assert_called_with(
            handle=f_gatt_controller.ServiceHandle(value=mock_handle),
            service=mock.ANY,
        )
        assert self.bluetooth_le_obj._remote_service_client is not None

    def test_discover_characteristics(self) -> None:
        """Test for BluetoothLE.discover_characteristics()."""
        self.bluetooth_le_obj._remote_service_client = mock.MagicMock()
        self.bluetooth_le_obj.loop = mock.MagicMock()
        self.bluetooth_le_obj.loop.run_until_complete = mock.MagicMock(
            return_value=_SAMPLE_DISCOVER_CHARACTERISTIC_RESPONSE
        )
        data = self.bluetooth_le_obj.discover_characteristics()
        self.assertEqual(data, _SAMPLE_DISCOVER_CHARACTERISTIC_OUTPUT)

    def test_read_characteristics(self) -> None:
        """Test for BluetoothLE.read_characteristics()."""
        self.bluetooth_le_obj._remote_service_client = mock.MagicMock()
        mock_handle = 1
        self.bluetooth_le_obj.loop = mock.MagicMock()
        mock_read_options = f_gatt_controller.ReadOptions()
        mock_read_options.short_read = f_gatt_controller.ShortReadOptions()
        mock_response = (
            f_gatt_controller.RemoteServiceReadCharacteristicResult()
        )
        mock_response.response = _SAMPLE_READ_CHARACTERISTIC_RESPONSE
        self.bluetooth_le_obj.loop.run_until_complete = mock.MagicMock(
            return_value=mock_response
        )
        data = self.bluetooth_le_obj.read_characteristic(handle=mock_handle)
        self.assertEqual(data, _SAMPLE_READ_CHARACTERISTIC_OUTPUT)
        self.bluetooth_le_obj._remote_service_client.read_characteristic.assert_called_with(
            handle=f_gatt_controller.ServiceHandle(value=mock_handle),
            options=mock_read_options,
        )


if __name__ == "__main__":
    unittest.main()
