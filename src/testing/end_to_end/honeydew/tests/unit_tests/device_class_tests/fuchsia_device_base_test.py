#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.device_classes.fuchsia_device_base.py."""

import unittest
from unittest import mock

from honeydew import errors
from honeydew.device_classes import fuchsia_device_base
from parameterized import parameterized

# pylint: disable=protected-access
_INPUT_ARGS = {
    "device_name": "fuchsia-emulator",
    "ssh_pkey": "/tmp/.ssh/pkey",
    "ssh_user": "root",
    "device_ip_address": "11.22.33.44"
}

_MOCK_ARGS = {
    "device_name": "fuchsia-emulator",
    "device_ip_address": "12.34.56.78",
    "device_type": "qemu-x64",
    "sl4f_request": fuchsia_device_base._SL4F_METHODS["GetDeviceName"],
    "sl4f_response": {
        'id': '',
        'result': 'fuchsia-emulator',
        'error': None,
    },
    "sl4f_error_response": {
        'id': '',
        'error': 'some error',
    },
}


def _custom_test_name_func(testcase_func, _, param):
    """Custom test name function method."""
    test_func_name = testcase_func.__name__

    params_dict = param.args[0]
    test_label = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FuchsiaDeviceBaseTests(unittest.TestCase):
    """Unit tests for honeydew.device_classes.fuchsia_device_base.py."""

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={"result": _INPUT_ARGS["device_name"]},
        autospec=True)
    @mock.patch.object(
        fuchsia_device_base.fuchsia_device.ffx_cli,
        "get_target_address",
        return_value=_MOCK_ARGS["device_ip_address"],
        autospec=True)
    @mock.patch.object(
        fuchsia_device_base.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True)
    def setUp(  # pylint: disable=arguments-differ
            self, mock_check_output, mock_get_target_address,
            mock_send_sl4f_command) -> None:
        super().setUp()

        self.fd_obj = fuchsia_device_base.FuchsiaDeviceBase(
            device_name=_INPUT_ARGS["device_name"],
            ssh_pkey=_INPUT_ARGS["ssh_pkey"])

        mock_get_target_address.assert_called_once()

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetDeviceName"])

        mock_check_output.assert_called_once()

    # List all the tests related to __init__ in alphabetical order
    @parameterized.expand(
        [
            (
                {
                    "label": "no_optional_params",
                    "device_name": _INPUT_ARGS["device_name"],
                    "optional_params": {},
                },),
            (
                {
                    "label": "all_optional_params",
                    "device_name": _INPUT_ARGS["device_name"],
                    "optional_params":
                        {
                            "ssh_pkey":
                                _INPUT_ARGS["ssh_pkey"],
                            "ssh_user":
                                _INPUT_ARGS["ssh_user"],
                            "device_ip_address":
                                _INPUT_ARGS["device_ip_address"]
                        },
                },),
            (
                {
                    "label": "no_ssh_user",
                    "device_name": _INPUT_ARGS["device_name"],
                    "optional_params":
                        {
                            "ssh_pkey":
                                _INPUT_ARGS["ssh_pkey"],
                            "device_ip_address":
                                _INPUT_ARGS["device_ip_address"]
                        },
                },),
            (
                {
                    "label": "just_ssh_key",
                    "device_name": _INPUT_ARGS["device_name"],
                    "optional_params": {
                        "ssh_pkey": _INPUT_ARGS["ssh_pkey"],
                    },
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        autospec=True)
    @mock.patch.object(
        fuchsia_device_base.fuchsia_device.ffx_cli,
        "get_target_address",
        autospec=True)
    @mock.patch.object(
        fuchsia_device_base.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True)
    def test_fuchsia_device_base_init(
            self, parameterized_dict, mock_check_output,
            mock_get_target_address, mock_send_sl4f_command):
        """Verify FuchsiaDeviceBase class instantiation."""
        optional_params = parameterized_dict["optional_params"]

        device_name = parameterized_dict["device_name"]

        mock_send_sl4f_command.return_value = {"result": device_name}

        fd_obj = fuchsia_device_base.FuchsiaDeviceBase(
            device_name=device_name, **optional_params)

        self.assertIsInstance(fd_obj, fuchsia_device_base.FuchsiaDeviceBase)

        mock_check_output.assert_called_once()

        mock_send_sl4f_command.assert_called_once_with(
            fd_obj, method=fuchsia_device_base._SL4F_METHODS["GetDeviceName"])

        if "device_ip_address" not in optional_params:
            mock_get_target_address.assert_called_once()
        else:
            mock_get_target_address.assert_not_called()

    # List all the tests related to static properties in alphabetical order
    @mock.patch.object(
        fuchsia_device_base.ffx_cli,
        "get_target_type",
        return_value=_MOCK_ARGS["device_type"],
        autospec=True)
    def test_device_type(self, mock_ffx_cli_get_target_type):
        """Testcase for FuchsiaDeviceBase.device_type property"""
        self.assertEqual(self.fd_obj.device_type, _MOCK_ARGS["device_type"])
        mock_ffx_cli_get_target_type.assert_called_once_with(self.fd_obj.name)

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_manufacturer(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase.manufacturer property"""
        self.assertEqual(self.fd_obj.manufacturer, "default-manufacturer")

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetProductInfo"])

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_model(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase.model property"""
        self.assertEqual(self.fd_obj.model, "default-model")

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetProductInfo"])

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_product_name(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase.product_name property"""
        self.assertEqual(self.fd_obj.product_name, "default-product-name")

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetProductInfo"])

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={"result": {
            "serial_number": "default-serial-number",
        }},
        autospec=True)
    def test_serial_number(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase.serial_number property"""
        self.assertEqual(self.fd_obj.serial_number, "default-serial-number")

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetDeviceInfo"])

    # List all the tests related to dynamic properties in alphabetical order
    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={"result": "1.2.3"},
        autospec=True)
    def test_firmware_version(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase.firmware_version property"""
        self.assertEqual(self.fd_obj.firmware_version, "1.2.3")

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj, method=fuchsia_device_base._SL4F_METHODS["GetVersion"])

    # List all the tests related to private methods in alphabetical order

    # Note - Test case for FuchsiaDeviceBase._start_sl4f_server() is covered in
    # test_fuchsia_device_base_init

    @mock.patch.object(
        fuchsia_device_base.FuchsiaDeviceBase,
        "_send_sl4f_command",
        return_value={"result": _MOCK_ARGS["device_name"]},
        autospec=True)
    def test_check_sl4f_connection(self, mock_send_sl4f_command):
        """Testcase for FuchsiaDeviceBase._check_sl4f_connection()"""
        self.fd_obj._check_sl4f_connection()

        mock_send_sl4f_command.assert_called_once_with(
            self.fd_obj,
            method=fuchsia_device_base._SL4F_METHODS["GetDeviceName"])

    @mock.patch.object(
        fuchsia_device_base.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True)
    def test_run_ssh_command_on_host(self, mock_check_output):
        """Testcase for FuchsiaDeviceBase._run_ssh_command_on_host()"""
        command = "some_command"

        self.assertEqual(
            self.fd_obj._run_ssh_command_on_host(command=command),
            "some output")

        mock_check_output.assert_called_once()

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
                            "message": "message"
                        },
                    },
                    "mock_http_response": _MOCK_ARGS["sl4f_response"],
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        fuchsia_device_base.http_utils,
        "send_http_request",
        return_value=_MOCK_ARGS["sl4f_response"],
        autospec=True)
    def test_send_sl4f_command_success(
            self, parameterized_dict, mock_send_http_request):
        """Testcase for FuchsiaDeviceBase._send_sl4f_command() success case"""
        method = parameterized_dict["method"]

        response = self.fd_obj._send_sl4f_command(method=method)

        self.assertEqual(response, parameterized_dict["mock_http_response"])

        mock_send_http_request.assert_called_once()

    @mock.patch.object(
        fuchsia_device_base.http_utils,
        "send_http_request",
        return_value=_MOCK_ARGS["sl4f_error_response"],
        autospec=True)
    def test_send_sl4f_command_fail_because_of_error_in_resp(
            self, mock_send_http_request):
        """Testcase for FuchsiaDeviceBase._send_sl4f_command() failure case when
        there is 'error' in SL4F response received."""
        method = _MOCK_ARGS["sl4f_request"]
        expected_error = _MOCK_ARGS['sl4f_error_response']['error']
        with self.assertRaisesRegex(errors.FuchsiaDeviceError,
                                    f"Error: '{expected_error}'"):
            self.fd_obj._send_sl4f_command(
                method=method, attempts=5, interval=0)

        self.assertEqual(mock_send_http_request.call_count, 5)

    @mock.patch.object(
        fuchsia_device_base.http_utils,
        "send_http_request",
        side_effect=RuntimeError("some run time error"),
        autospec=True)
    def test_send_sl4f_command_fail_because_of_exception(
            self, mock_send_http_request):
        """Testcase for FuchsiaDeviceBase._send_sl4f_command() failure case when
        there is an exception thrown while sending HTTP request."""
        method = _MOCK_ARGS["sl4f_request"]
        with self.assertRaisesRegex(
                errors.FuchsiaDeviceError,
                f"SL4F method '{method}' failed on '{self.fd_obj.name}'."):
            self.fd_obj._send_sl4f_command(
                method=method, attempts=5, interval=0)

        mock_send_http_request.assert_called_once()


if __name__ == '__main__':
    unittest.main()
