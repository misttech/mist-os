#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.fuchsia_device.sl4f.fuchsia_device.py."""

import base64
import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller
from parameterized import parameterized

from honeydew import custom_types
from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.fuchsia_device.sl4f import fuchsia_device
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)

_INPUT_ARGS: dict[str, Any] = {
    "device_name": "fuchsia-emulator",
    "ssh_private_key": "/tmp/.ssh/pkey",
    "ssh_user": "root",
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
        logs_dir="/tmp/logs",
        binary_path="/bin/ffx",
        logs_level="debug",
        mdns_enabled=False,
        subtools_search_path=None,
    ),
}

_MOCK_DEVICE_PROPERTIES: dict[str, dict[str, str]] = {
    "build_info": {
        "result": "123456",
    },
    "device_info": {
        "serial_number": "123456",
    },
    "product_info": {
        "manufacturer": "default-manufacturer",
        "model": "default-model",
        "name": "default-product-name",
    },
}

_BASE64_ENCODED_STR: str = "some base64 encoded string=="


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FuchsiaDeviceSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.fuchsia_device.sl4f.fuchsia_device.py."""

    def __init__(self, *args, **kwargs) -> None:
        self.fd_obj: fuchsia_device.FuchsiaDevice
        super().__init__(*args, **kwargs)

    def setUp(self) -> None:
        super().setUp()

        with mock.patch.object(
            fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True
        ) as mock_sl4f_start_server, mock.patch.object(
            fuchsia_device.sl4f_transport.SL4F,
            "check_connection",
            autospec=True,
        ) as mock_sl4f_check_connection, mock.patch.object(
            base_fuchsia_device.ssh_transport.SSH,
            "check_connection",
            autospec=True,
        ) as mock_ssh_check_connection, mock.patch.object(
            base_fuchsia_device.ffx_transport.FFX,
            "check_connection",
            autospec=True,
        ) as mock_ffx_check_connection:
            self.fd_obj = fuchsia_device.FuchsiaDevice(
                device_name=_INPUT_ARGS["device_name"],
                ssh_private_key=_INPUT_ARGS["ssh_private_key"],
                ffx_config=_INPUT_ARGS["ffx_config"],
            )

            mock_ffx_check_connection.assert_called()
            mock_ssh_check_connection.assert_called()
            mock_sl4f_start_server.assert_called()
            mock_sl4f_check_connection.assert_called()

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice
        )

    # List all the tests related to public methods
    def test_close(self) -> None:
        """Testcase for FuchsiaDevice.close()"""
        self.fd_obj.close()

    # List all the tests related to private methods
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": _MOCK_DEVICE_PROPERTIES["build_info"]},
        autospec=True,
    )
    def test_build_info(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice._build_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._build_info,
            {"version": _MOCK_DEVICE_PROPERTIES["build_info"]},
        )
        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": _MOCK_DEVICE_PROPERTIES["device_info"]},
        autospec=True,
    )
    def test_device_info(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice._device_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._device_info, _MOCK_DEVICE_PROPERTIES["device_info"]
        )
        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": _MOCK_DEVICE_PROPERTIES["product_info"]},
        autospec=True,
    )
    def test_product_info(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice._product_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._product_info, _MOCK_DEVICE_PROPERTIES["product_info"]
        )
        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True
    )
    def test_on_device_boot(
        self, mock_sl4f_start_server, mock_health_check
    ) -> None:
        """Testcase for FuchsiaDevice.on_device_boot()"""
        self.fd_obj.on_device_boot()

        mock_health_check.assert_called()
        mock_sl4f_start_server.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "info_level",
                    "log_level": custom_types.LEVEL.INFO,
                    "log_message": "info message",
                },
            ),
            (
                {
                    "label": "warning_level",
                    "log_level": custom_types.LEVEL.WARNING,
                    "log_message": "warning message",
                },
            ),
            (
                {
                    "label": "error_level",
                    "log_level": custom_types.LEVEL.ERROR,
                    "log_message": "error message",
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(fuchsia_device.sl4f_transport.SL4F, "run", autospec=True)
    def test_send_log_command(self, parameterized_dict, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice._send_log_command()"""
        # pylint: disable=protected-access
        self.fd_obj._send_log_command(
            tag="test",
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"],
        )

        mock_sl4f_run.assert_called()

    @mock.patch.object(fuchsia_device.sl4f_transport.SL4F, "run", autospec=True)
    def test_send_reboot_command(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command()"""
        # pylint: disable=protected-access
        self.fd_obj._send_reboot_command()

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": {"zip": _BASE64_ENCODED_STR}},
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True
    )
    def test_send_snapshot_command(
        self, mock_sl4f_start_server, mock_health_check, mock_sl4f_run
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command()"""
        # pylint: disable=protected-access
        base64_bytes = self.fd_obj._send_snapshot_command()

        self.assertEqual(base64_bytes, base64.b64decode(_BASE64_ENCODED_STR))

        mock_sl4f_start_server.assert_called()
        mock_health_check.assert_called()
        mock_sl4f_run.assert_called()


if __name__ == "__main__":
    unittest.main()
