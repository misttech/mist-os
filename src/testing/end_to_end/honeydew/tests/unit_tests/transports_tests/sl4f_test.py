#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.sl4f.py."""

from typing import Any, Dict
import unittest
from unittest import mock

from parameterized import parameterized

from honeydew import custom_types
from honeydew import errors
from honeydew.transports import sl4f

# pylint: disable=protected-access

_INPUT_ARGS: Dict[str, Any] = {
    "device_name": "fuchsia-emulator",
}

_IPV4 = "11.22.33.44"
_IPV6 = "fe80::4fce:3102:ef13:888c%qemu"
_IPV6_WITH_SCOPE = _IPV6
_IPV6_WO_SCOPE = "fe80::4fce:3102:ef13:888c"

_IPV6_LOCALHOST = "::1"
_SL4F_PORT_LOCAL = sl4f._SL4F_PORT["LOCAL"]
_SL4F_PORT_REMOTE = sl4f._SL4F_PORT["REMOTE"]
_SSH_PORT = 22

_MOCK_ARGS: Dict[str, Any] = {
    "device_name":
        "fuchsia-emulator",
    "invalid-device_name":
        "invalid-device_name",
    "sl4f_server_address_ipv4":
        custom_types.Sl4fServerAddress(ip=_IPV4, port=_SL4F_PORT_LOCAL),
    "sl4f_server_address_ipv6":
        custom_types.Sl4fServerAddress(ip=_IPV6, port=_SL4F_PORT_LOCAL),
    "sl4f_server_address_ipv6_localhost":
        custom_types.Sl4fServerAddress(
            ip=_IPV6_LOCALHOST, port=_SL4F_PORT_REMOTE),
    "target_ssh_address_ipv4":
        custom_types.TargetSshAddress(ip=_IPV4, port=_SSH_PORT),
    "target_ssh_address_ipv6":
        custom_types.TargetSshAddress(ip=_IPV6, port=_SSH_PORT),
    "target_ssh_address_ipv6_localhost":
        custom_types.TargetSshAddress(ip=_IPV6_LOCALHOST, port=_SSH_PORT),
    "sl4f_request":
        sl4f._SL4F_METHODS["GetDeviceName"],
    "sl4f_response": {
        "id": "",
        "result": "fuchsia-emulator",
        "error": None,
    },
    "sl4f_error_response": {
        "id": "",
        "error": "some error",
    },
    "sl4f_url_v4":
        "http://{_IPV4}:{_SL4F_PORT_LOCAL}",
}

_EXPECTED_VALUES: Dict[str, Any] = {
    "url_ipv4":
        f"http://{_IPV4}:{_SL4F_PORT_LOCAL}",
    "url_ipv6":
        f"http://[{_IPV6}]:{_SL4F_PORT_LOCAL}",
    "url_ipv6_localhost":
        f"http://[{_IPV6_LOCALHOST}]:{_SL4F_PORT_REMOTE}",
    "sl4f_server_address_ipv4":
        custom_types.Sl4fServerAddress(ip=_IPV4, port=_SL4F_PORT_LOCAL),
    "sl4f_server_address_ipv6":
        custom_types.Sl4fServerAddress(ip=_IPV6, port=_SL4F_PORT_LOCAL),
    "sl4f_server_address_ipv6_localhost":
        custom_types.Sl4fServerAddress(
            ip=_IPV6_LOCALHOST, port=_SL4F_PORT_REMOTE),
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class Sl4fTests(unittest.TestCase):
    """Unit tests for honeydew.transports.sl4f.py."""

    @mock.patch.object(sl4f.SL4F, "start_server", autospec=True)
    def setUp(self, mock_sl4f_start_server) -> None:
        super().setUp()

        self.sl4f_obj = sl4f.SL4F(device_name=_INPUT_ARGS["device_name"])

        mock_sl4f_start_server.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label":
                        "ipv4_address",
                    "sl4f_server_address":
                        _MOCK_ARGS["sl4f_server_address_ipv4"],
                    "expected_url":
                        _EXPECTED_VALUES["url_ipv4"],
                },),
            (
                {
                    "label":
                        "ipv6_address",
                    "sl4f_server_address":
                        _MOCK_ARGS["sl4f_server_address_ipv6"],
                    "expected_url":
                        _EXPECTED_VALUES["url_ipv6"],
                },),
            (
                {
                    "label":
                        "ipv6_localhost_address",
                    "sl4f_server_address":
                        _MOCK_ARGS["sl4f_server_address_ipv6_localhost"],
                    "expected_url":
                        _EXPECTED_VALUES["url_ipv6_localhost"],
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(sl4f.SL4F, "_get_sl4f_server_address", autospec=True)
    def test_sl4f_url(
            self, parameterized_dict, mock_get_sl4f_server_address) -> None:
        """Testcase for SL4F.url property.

        It also tests SL4F._get_ip_version()."""
        mock_get_sl4f_server_address.return_value = \
            parameterized_dict["sl4f_server_address"]

        self.assertEqual(self.sl4f_obj.url, parameterized_dict["expected_url"])

        mock_get_sl4f_server_address.assert_called()

    @mock.patch.object(
        sl4f.SL4F,
        "run",
        return_value={"result": _MOCK_ARGS["device_name"]},
        autospec=True)
    def test_check_connection(self, mock_sl4f_run) -> None:
        """Testcase for SL4F.check_connection()"""
        self.sl4f_obj.check_connection()

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        sl4f.SL4F,
        "run",
        return_value={"result": _MOCK_ARGS["invalid-device_name"]},
        autospec=True)
    def test_check_connection_exception(self, mock_sl4f_run) -> None:
        """Testcase for SL4F.check_connection() raising exception"""
        with self.assertRaises(errors.Sl4fError):
            self.sl4f_obj.check_connection()

        mock_sl4f_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "just_mandatory_method_arg",
                    "method": _MOCK_ARGS["sl4f_request"],
                    "optional_params": {},
                    "mock_http_response": _MOCK_ARGS["sl4f_response"],
                },),
            (
                {
                    "label": "optional_params_arg",
                    "method": _MOCK_ARGS["sl4f_request"],
                    "optional_params": {
                        "params": {
                            "message": "message",
                        },
                    },
                    "mock_http_response": _MOCK_ARGS["sl4f_response"],
                },),
            (
                {
                    "label": "all_optional_params",
                    "method": _MOCK_ARGS["sl4f_request"],
                    "optional_params":
                        {
                            "params": {
                                "message": "message",
                            },
                            "timeout": 3,
                            "attempts": 3,
                            "interval": 3,
                            "exceptions_to_skip": []
                        },
                    "mock_http_response": _MOCK_ARGS["sl4f_response"],
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        sl4f.http_utils,
        "send_http_request",
        return_value=_MOCK_ARGS["sl4f_response"],
        autospec=True)
    @mock.patch.object(
        sl4f.SL4F,
        "url",
        new_callable=mock.PropertyMock,
        return_value=_MOCK_ARGS["sl4f_url_v4"])
    def test_sl4f_run(
            self, parameterized_dict, mock_sl4f_url,
            mock_send_http_request) -> None:
        """Testcase for SL4F.run() success case"""
        method: str = parameterized_dict["method"]
        optional_params: Dict[str, Any] = parameterized_dict["optional_params"]

        response: Dict[str, Any] = self.sl4f_obj.run(
            method=method, **optional_params)

        self.assertEqual(response, parameterized_dict["mock_http_response"])

        mock_sl4f_url.assert_called()
        mock_send_http_request.assert_called()

    @mock.patch.object(
        sl4f.http_utils,
        "send_http_request",
        return_value=_MOCK_ARGS["sl4f_error_response"],
        autospec=True)
    @mock.patch.object(
        sl4f.SL4F,
        "url",
        new_callable=mock.PropertyMock,
        return_value=_MOCK_ARGS["sl4f_url_v4"])
    def test_sl4f_run_fail_because_of_error_in_resp(
            self, mock_sl4f_url, mock_send_http_request) -> None:
        """Testcase for SL4F.run() failure case when there is 'error' in SL4F
        response received"""
        with self.assertRaises(errors.Sl4fError):
            self.sl4f_obj.run(
                method=_MOCK_ARGS["sl4f_request"], attempts=5, interval=0)

        mock_sl4f_url.assert_called()
        mock_send_http_request.assert_called()

    @mock.patch.object(
        sl4f.http_utils,
        "send_http_request",
        side_effect=RuntimeError("some run time error"),
        autospec=True)
    @mock.patch.object(
        sl4f.SL4F,
        "url",
        new_callable=mock.PropertyMock,
        return_value=_MOCK_ARGS["sl4f_url_v4"])
    def test_send_sl4f_command_fail_because_of_exception(
            self, mock_sl4f_url, mock_send_http_request) -> None:
        """Testcase for SL4F.run() failure case when there is an exception
        thrown while sending HTTP request"""
        with self.assertRaises(errors.Sl4fError):
            self.sl4f_obj.run(
                method=_MOCK_ARGS["sl4f_request"], attempts=5, interval=0)

        mock_sl4f_url.assert_called()
        mock_send_http_request.assert_called_once()

    @mock.patch.object(sl4f.SL4F, "check_connection", autospec=True)
    @mock.patch.object(sl4f.ffx_transport.FFX, "run", autospec=True)
    def test_start_server(self, mock_ffx_run, mock_check_connection) -> None:
        """Testcase for SL4F.start_server()"""
        self.sl4f_obj.start_server()

        mock_ffx_run.assert_called()
        mock_check_connection.assert_called()

    @mock.patch.object(
        sl4f.ffx_transport.FFX,
        "run",
        side_effect=errors.FfxCommandError("error"),
        autospec=True)
    def test_start_server_exception(self, mock_ffx_run) -> None:
        """Testcase for SL4F.start_server() raising exception"""
        with self.assertRaises(errors.Sl4fError):
            self.sl4f_obj.start_server()

        mock_ffx_run.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label":
                        "ipv4",
                    "target_ssh_address":
                        _MOCK_ARGS["target_ssh_address_ipv4"],
                    "expected_sl4f_address":
                        _EXPECTED_VALUES["sl4f_server_address_ipv4"],
                },),
            (
                {
                    "label":
                        "ipv6",
                    "target_ssh_address":
                        _MOCK_ARGS["target_ssh_address_ipv6"],
                    "expected_sl4f_address":
                        _EXPECTED_VALUES["sl4f_server_address_ipv6"],
                },),
            (
                {
                    "label":
                        "ipv6_localhost",
                    "target_ssh_address":
                        _MOCK_ARGS["target_ssh_address_ipv6_localhost"],
                    "expected_sl4f_address":
                        _EXPECTED_VALUES["sl4f_server_address_ipv6_localhost"],
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        sl4f.ffx_transport.FFX, "get_target_ssh_address", autospec=True)
    def test_get_sl4f_server_address(
            self, parameterized_dict, mock_get_target_ssh_address) -> None:
        """Testcase for SL4F._get_sl4f_server_address()"""
        mock_get_target_ssh_address.return_value = parameterized_dict[
            "target_ssh_address"]

        self.assertEqual(
            self.sl4f_obj._get_sl4f_server_address(),
            parameterized_dict["expected_sl4f_address"])

        mock_get_target_ssh_address.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "ipv4",
                    "py_ver": (3, 10),
                    "ip": _IPV4,
                    "expected": _IPV4,
                },),
            (
                {
                    "label": "ipv6_with_scope_id_on_py38",
                    "py_ver": (3, 8),
                    "ip": _IPV6_WITH_SCOPE,
                    "expected": _IPV6_WO_SCOPE,
                },),
            (
                {
                    "label": "ipv6_with_scope_id_on_py310",
                    "py_ver": (3, 10),
                    "ip": _IPV6_WITH_SCOPE,
                    "expected": _IPV6_WITH_SCOPE,
                },),
            (
                {
                    "label": "ipv6_without_scope_id",
                    "py_ver": (3, 10),
                    "ip": _IPV6_WO_SCOPE,
                    "expected": _IPV6_WO_SCOPE,
                },),
        ],
        name_func=_custom_test_name_func)
    def test_normalize_ip_addr(self, parameterized_dict) -> None:
        """Test case for FuchsiaDeviceBase._normalize_ip_addr()"""
        with mock.patch.object(sl4f.sys, "version_info",
                               parameterized_dict["py_ver"]):
            self.assertEqual(
                self.sl4f_obj._normalize_ip_addr(parameterized_dict["ip"]),
                parameterized_dict["expected"])
