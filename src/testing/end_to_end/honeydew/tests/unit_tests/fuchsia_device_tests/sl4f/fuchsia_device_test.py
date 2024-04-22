#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.fuchsia_device.sl4f.fuchsia_device.py."""

import base64
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller
from parameterized import param, parameterized

from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.fuchsia_device.sl4f import fuchsia_device
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.typing import custom_types

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


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FuchsiaDeviceSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.fuchsia_device.sl4f.fuchsia_device.py."""

    def __init__(self, *args, **kwargs) -> None:
        self.fd_obj: fuchsia_device.FuchsiaDevice
        super().__init__(*args, **kwargs)

    def setUp(self) -> None:
        super().setUp()

        with (
            mock.patch.object(
                fuchsia_device.sl4f_transport.SL4F,
                "start_server",
                autospec=True,
            ) as mock_sl4f_start_server,
            mock.patch.object(
                fuchsia_device.sl4f_transport.SL4F,
                "check_connection",
                autospec=True,
            ) as mock_sl4f_check_connection,
            mock.patch.object(
                base_fuchsia_device.ssh_transport.SSH,
                "check_connection",
                autospec=True,
            ) as mock_ssh_check_connection,
            mock.patch.object(
                base_fuchsia_device.ffx_transport.FFX,
                "check_connection",
                autospec=True,
            ) as mock_ffx_check_connection,
        ):
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

    # List all the tests related to transports
    def test_sl4f_transport(self) -> None:
        """Test case to make sure fsl4f_fuchsia_device supports SL4F
        transport."""
        self.assertIsInstance(
            self.fd_obj.sl4f, fuchsia_device.sl4f_transport.SL4F
        )

    # List all the tests related to affordances
    @mock.patch.object(
        fuchsia_device.bluetooth_avrcp_sl4f.bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_avrcp(
        self, mock_bluetooth_common_init: mock.Mock
    ) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        bluetooth_avrcp affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.bluetooth_avrcp,
            fuchsia_device.bluetooth_avrcp_sl4f.BluetoothAvrcp,
        )
        mock_bluetooth_common_init.assert_called_once()

    @mock.patch.object(
        fuchsia_device.bluetooth_avrcp_sl4f.bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_gap(self, mock_bluetooth_common_init: mock.Mock) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        bluetooth_gap affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.bluetooth_gap,
            fuchsia_device.bluetooth_gap_sl4f.BluetoothGap,
        )
        mock_bluetooth_common_init.assert_called_once()

    def test_rtc(self) -> None:
        """Test case to make sure sl4f_fuchsia_device does not support rtc
        affordance"""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.rtc  # pylint: disable=pointless-statement

    def test_tracing(self) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        tracing affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.tracing,
            fuchsia_device.tracing_sl4f.Tracing,
        )

    def test_user_input(self) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        user_input affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.user_input,
            fuchsia_device.user_input_sl4f.UserInput,
        )

    def test_wlan_policy(self) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        wlan_policy affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.wlan_policy,
            fuchsia_device.wlan_policy_sl4f.WlanPolicy,
        )

    def test_wlan(self) -> None:
        """Test case to make sure sl4f_fuchsia_device supports
        wlan affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.wlan,
            fuchsia_device.wlan_sl4f.Wlan,
        )

    # List all the tests related to public methods
    def test_close(self) -> None:
        """Testcase for FuchsiaDevice.close()"""
        self.fd_obj.close()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.base_fuchsia_device.BaseFuchsiaDevice,
        "health_check",
        autospec=True,
    )
    def test_health_check(
        self,
        mock_base_fuchsia_device_health_check: mock.Mock,
        mock_sl4f_check_connection: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.health_check()"""
        self.fd_obj.health_check()
        mock_base_fuchsia_device_health_check.assert_called_once_with(
            self.fd_obj
        )
        mock_sl4f_check_connection.assert_called_once_with(self.fd_obj.sl4f)

    @mock.patch.object(
        fuchsia_device.base_fuchsia_device.BaseFuchsiaDevice,
        "on_device_boot",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    def test_on_device_boot(
        self,
        mock_sl4f_start_server: mock.Mock,
        mock_sl4f_based_health_check: mock.Mock,
        mock_base_fuchsia_device_on_device_boot: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.on_device_boot()"""
        self.fd_obj.on_device_boot()

        mock_sl4f_start_server.assert_called_once_with(self.fd_obj.sl4f)
        mock_sl4f_based_health_check.assert_called_once_with(self.fd_obj)
        mock_base_fuchsia_device_on_device_boot.assert_called_once_with(
            self.fd_obj
        )

    # List all the tests related to private methods
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": _MOCK_DEVICE_PROPERTIES["build_info"]},
        autospec=True,
    )
    def test_build_info(self, mock_sl4f_run: mock.Mock) -> None:
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
    def test_device_info(self, mock_sl4f_run: mock.Mock) -> None:
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
    def test_product_info(self, mock_sl4f_run: mock.Mock) -> None:
        """Testcase for FuchsiaDevice._product_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._product_info, _MOCK_DEVICE_PROPERTIES["product_info"]
        )
        mock_sl4f_run.assert_called()

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
    def test_send_log_command(
        self, parameterized_dict: dict[str, Any], mock_sl4f_run: mock.Mock
    ) -> None:
        """Testcase for FuchsiaDevice._send_log_command()"""
        # pylint: disable=protected-access
        self.fd_obj._send_log_command(
            tag="test",
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"],
        )

        mock_sl4f_run.assert_called()

    @mock.patch.object(fuchsia_device.sl4f_transport.SL4F, "run", autospec=True)
    def test_send_reboot_command(self, mock_sl4f_run: mock.Mock) -> None:
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
        self,
        mock_sl4f_start_server: mock.Mock,
        mock_health_check: mock.Mock,
        mock_sl4f_run: mock.Mock,
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
