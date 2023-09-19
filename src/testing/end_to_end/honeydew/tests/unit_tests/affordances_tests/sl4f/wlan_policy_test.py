#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.wlan_policy.py."""

from typing import Any
import unittest
from unittest import mock

from parameterized import parameterized

from honeydew.affordances.sl4f.wlan import wlan_policy as sl4f_wlan_policy
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing.wlan import ConnectionState
from honeydew.typing.wlan import DisconnectStatus
from honeydew.typing.wlan import NetworkConfig
from honeydew.typing.wlan import NetworkIdentifier
from honeydew.typing.wlan import NetworkState
from honeydew.typing.wlan import SecurityType
from honeydew.typing.wlan import WlanClientState


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class WlanPolicySL4FTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.sl4f.wlan.wlan_policy.py."""

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.wlan_obj = sl4f_wlan_policy.WlanPolicy(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj)
        self.sl4f_obj.reset_mock()

    def test_create_client_controller(self) -> None:
        """Test for WlanPolicy.create_client_controller()."""

        self.wlan_obj.create_client_controller()

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label":
                        "success_case",
                    "return_value":
                        {
                            "result":
                                [
                                    {
                                        "ssid": "1234abcd",
                                        "security_type": "wep",
                                        "credential_type": "password",
                                        "credential_value": "helloWorld",
                                    },
                                    {
                                        "ssid": "test",
                                        "security_type": "wpa3",
                                        "credential_type": "password",
                                        "credential_value": "helloWorld",
                                    },
                                ],
                        },
                    "expected_value":
                        [
                            NetworkConfig(
                                "1234abcd", SecurityType.WEP, "password",
                                "helloWorld"),
                            NetworkConfig(
                                "test", SecurityType.WPA3, "password",
                                "helloWorld"),
                        ],
                },),
            (
                {
                    "label": "response_not_list",
                    "return_value": {
                        "result": "not_list"
                    },
                },),
            (
                {
                    "label": "list_obj_not_dict",
                    "return_value": {
                        "result": [1, True, False],
                    },
                },),
            (
                {
                    "label": "dict_value_not_str",
                    "return_value":
                        {
                            "result":
                                [
                                    {
                                        "ssid": 1,
                                        "security_type": True,
                                        "credential_type": [],
                                        "credential_value": 2,
                                    },
                                ],
                        },
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_saved_networks(self, parameterized_dict) -> None:
        """Test for WlanPolicy.get_saved_networks()."""
        self.sl4f_obj.run.return_value = parameterized_dict["return_value"]

        if not isinstance(parameterized_dict["return_value"]["result"], list):
            with self.assertRaises(TypeError):
                self.wlan_obj.get_saved_networks()
        elif not isinstance(parameterized_dict["return_value"]["result"][0],
                            dict):
            with self.assertRaises(TypeError):
                self.wlan_obj.get_saved_networks()
        elif not isinstance(
                parameterized_dict["return_value"]["result"][0]["ssid"], str):
            with self.assertRaises(TypeError):
                self.wlan_obj.get_saved_networks()
        else:
            self.assertEqual(
                self.wlan_obj.get_saved_networks(),
                parameterized_dict["expected_value"])

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "success_case",
                    "return_value":
                        {
                            "result":
                                {
                                    "state":
                                        "ConnectionsEnabled",
                                    "networks":
                                        [
                                            {
                                                "id":
                                                    {
                                                        "ssid": "test_b",
                                                        "type_": "wep",
                                                    },
                                                "state": "Connecting",
                                                "status": "ConnectionFailed",
                                            },
                                            {
                                                "id":
                                                    {
                                                        "ssid": "test_a",
                                                        "type_": "none",
                                                    },
                                                "state": "Failed",
                                                "status": "TimedOut",
                                            },
                                        ],
                                },
                        },
                    "expected_value":
                        {
                            "state":
                                WlanClientState.CONNECTIONS_ENABLED,
                            "networks":
                                [
                                    NetworkState(
                                        NetworkIdentifier(
                                            "test_b", SecurityType.WEP),
                                        ConnectionState.CONNECTING,
                                        DisconnectStatus.CONNECTION_FAILED),
                                    NetworkState(
                                        NetworkIdentifier(
                                            "test_a", SecurityType.NONE),
                                        ConnectionState.FAILED,
                                        DisconnectStatus.TIMED_OUT),
                                ],
                        },
                },),
            (
                {
                    "label": "response_not_dict",
                    "return_value": {
                        "result": "not_dict",
                    },
                },),
            (
                {
                    "label": "response_networks_not_list",
                    "return_value": {
                        "result": {
                            "networks": "not_list",
                        }
                    },
                },),
            (
                {
                    "label": "network_no_state_status",
                    "return_value":
                        {
                            "result":
                                {
                                    "state":
                                        "ConnectionsEnabled",
                                    "networks":
                                        [
                                            {
                                                "id":
                                                    {
                                                        "ssid": "fail",
                                                        "type_": "wep",
                                                    },
                                                "state": None,
                                                "status": None,
                                            },
                                        ],
                                },
                        },
                    "expected_value":
                        {
                            "state":
                                WlanClientState.CONNECTIONS_ENABLED,
                            "networks":
                                [
                                    NetworkState(
                                        NetworkIdentifier(
                                            "fail", SecurityType.WEP),
                                        ConnectionState.DISCONNECTED,
                                        DisconnectStatus.CONNECTION_STOPPED),
                                ],
                        },
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_update(self, parameterized_dict) -> None:
        """Testcase for WlanPolicy.get_update()"""
        self.sl4f_obj.run.return_value = parameterized_dict["return_value"]

        if not isinstance(parameterized_dict["return_value"]["result"], dict):
            with self.assertRaises(TypeError):
                self.wlan_obj.get_update()
        elif not isinstance(
                parameterized_dict["return_value"]["result"]["networks"], list):
            with self.assertRaises(TypeError):
                self.wlan_obj.get_update()
        else:
            resp = self.wlan_obj.get_update()

            self.assertEqual(
                resp.state, parameterized_dict["expected_value"]["state"])
            self.assertEqual(
                resp.networks, parameterized_dict["expected_value"]["networks"])

        self.sl4f_obj.run.assert_called()

    def test_remove_all_networks(self) -> None:
        """Test for WlanPolicy.remove_all_networks()."""

        self.wlan_obj.remove_all_networks()

        self.sl4f_obj.run.assert_called()

    def test_save_network(self) -> None:
        """Test for WlanPolicy.save_network()."""

        self.wlan_obj.save_network(
            target_ssid="test", security_type=SecurityType.NONE)

        self.sl4f_obj.run.assert_called()

    def test_set_new_update_listener(self) -> None:
        """Test for WlanPolicy.set_new_update_listener()."""

        self.wlan_obj.set_new_update_listener()

        self.sl4f_obj.run.assert_called()

    def test_start_client_connections(self) -> None:
        """Test for WlanPolicy.start_client_connections()."""

        self.wlan_obj.start_client_connections()

        self.sl4f_obj.run.assert_called()

    def test_stop_client_connections(self) -> None:
        """Test for WlanPolicy.stop_client_connections()."""

        self.wlan_obj.stop_client_connections()

        self.sl4f_obj.run.assert_called()


if __name__ == "__main__":
    unittest.main()
