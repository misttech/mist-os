#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for
honeydew.device_classes.fuchsia_controller.fuchsia_device.py."""

from typing import Any, Dict
import unittest
from unittest import mock

import fidl.fuchsia_buildinfo as f_buildinfo
import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fidl.fuchsia_hardware_power_statecontrol as fhp_statecontrol
import fidl.fuchsia_hwinfo as f_hwinfo
import fuchsia_controller_py as fuchsia_controller
from parameterized import parameterized

from honeydew import custom_types
from honeydew import errors
from honeydew.device_classes import base_fuchsia_device
from honeydew.device_classes.fuchsia_controller import fuchsia_device
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import \
    fuchsia_device as fuchsia_device_interface

_INPUT_ARGS: Dict[str, str] = {
    "device_name": "fuchsia-emulator",
    "ssh_private_key": "/tmp/.ssh/pkey",
    "ssh_user": "root",
}

_MOCK_DEVICE_PROPERTIES: Dict[str, Dict[str, Any]] = {
    "build_info": {
        "version": "123456",
    },
    "device_info": {
        "serial_number": "123456",
    },
    "product_info":
        {
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
}

_MOCK_ARGS: Dict[str, Any] = {
    "ffx_config":
        custom_types.FFXConfig(
            isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
            logs_dir="/tmp/logs"),
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FuchsiaDeviceFCTests(unittest.TestCase):
    """Unit tests for
    honeydew.device_classes.fuchsia_controller.fuchsia_device.py."""

    def __init__(self, *args, **kwargs) -> None:
        self.fd_obj: fuchsia_device.FuchsiaDevice
        super().__init__(*args, **kwargs)

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH,
        "check_connection",
        autospec=True)
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    @mock.patch.object(
        fuchsia_device.ffx_transport,
        "get_config",
        return_value=_MOCK_ARGS["ffx_config"],
        autospec=True)
    def setUp(
            self, mock_ffx_get_config, mock_fc_context,
            mock_ssh_check_connection, mock_ffx_check_connection) -> None:
        super().setUp()

        self.fd_obj = fuchsia_device.FuchsiaDevice(
            device_name=_INPUT_ARGS["device_name"],
            ssh_private_key=_INPUT_ARGS["ssh_private_key"])

        mock_ffx_get_config.assert_called_once()
        mock_fc_context.assert_called_once_with(
            config=mock.ANY,
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            target=self.fd_obj.device_name)
        mock_ffx_check_connection.assert_called_once_with(self.fd_obj.ffx)
        mock_ssh_check_connection.assert_called_once_with(self.fd_obj.ssh)

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice)

    # List all the tests related to affordances in alphabetical order
    def test_fuchsia_device_is_bluetooth_gap_capable(self) -> None:
        """Test case to make sure fuchsia device is BluetoothGap capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.BluetoothGapCapableDevice)

    def test_fuchsia_device_is_component_capable(self) -> None:
        """Test case to make sure fuchsia device is component capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.ComponentCapableDevice)

    def test_fuchsia_device_is_reboot_capable(self) -> None:
        """Test case to make sure fuchsia device is reboot capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.RebootCapableDevice)

    def test_fuchsia_device_is_tracing_capable(self) -> None:
        """Test case to make sure fuchsia device is tracing capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.TracingCapableDevice)

    # List all the tests related to public methods in alphabetical order
    def test_close(self) -> None:
        """Testcase for FuchsiaDevice.close()"""
        self.fd_obj.close()

    # List all the tests related to private properties in alphabetical order
    @mock.patch.object(
        f_buildinfo.Provider.Client,
        "get_build_info",
        new_callable=mock.AsyncMock,
        return_value=f_buildinfo.ProviderGetBuildInfoResponse(
            build_info=_MOCK_DEVICE_PROPERTIES["build_info"]),
    )
    def test_build_info(self, mock_buildinfo_provider) -> None:
        """Testcase for FuchsiaDevice._build_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._build_info, _MOCK_DEVICE_PROPERTIES["build_info"])
        mock_buildinfo_provider.assert_called()

    @mock.patch.object(
        f_buildinfo.Provider.Client,
        "get_build_info",
        new_callable=mock.AsyncMock,
        return_value=f_buildinfo.ProviderGetBuildInfoResponse(
            build_info=_MOCK_DEVICE_PROPERTIES["build_info"]),
    )
    def test_build_info_error(self, mock_buildinfo_provider) -> None:
        """Testcase for FuchsiaDevice._build_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_buildinfo_provider.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _ = self.fd_obj._build_info

    @mock.patch.object(
        f_hwinfo.Device.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.DeviceGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["device_info"]),
    )
    def test_device_info(self, mock_hwinfo_device) -> None:
        """Testcase for FuchsiaDevice._device_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._device_info, _MOCK_DEVICE_PROPERTIES["device_info"])
        mock_hwinfo_device.assert_called()

    @mock.patch.object(
        f_hwinfo.Device.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.DeviceGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["device_info"]),
    )
    def test_device_info_error(self, mock_hwinfo_device) -> None:
        """Testcase for FuchsiaDevice._device_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_hwinfo_device.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _ = self.fd_obj._device_info

    @mock.patch.object(
        f_hwinfo.Product.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.ProductGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["product_info"]),
    )
    def test_product_info(self, mock_hwinfo_product) -> None:
        """Testcase for FuchsiaDevice._product_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_obj._product_info, _MOCK_DEVICE_PROPERTIES["product_info"])
        mock_hwinfo_product.assert_called()

    @mock.patch.object(
        f_hwinfo.Product.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.ProductGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["product_info"]),
    )
    def test_product_info_error(self, mock_hwinfo_product) -> None:
        """Testcase for FuchsiaDevice._product_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_hwinfo_product.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _ = self.fd_obj._product_info

    # List all the tests related to private methods in alphabetical order
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    def test_context_create_init_error(self, mock_fc_context) -> None:
        """Verify _context_create when the fuchsia controller Context creation
        raises an error."""
        mock_fc_context.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            self.fd_obj._context_create()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True)
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    @mock.patch.object(
        fuchsia_device.ffx_transport,
        "get_config",
        return_value=_MOCK_ARGS["ffx_config"],
        autospec=True)
    def test_on_device_boot(
            self, mock_ffx_get_config, mock_fc_context,
            mock_health_check) -> None:
        """Testcase for FuchsiaDevice._on_device_boot()"""
        # pylint: disable=protected-access
        self.fd_obj.on_device_boot()

        mock_ffx_get_config.assert_called_once()
        mock_fc_context.assert_called_once_with(
            config=mock.ANY,
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            target=self.fd_obj.device_name)
        mock_health_check.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "info_level",
                    "log_level": custom_types.LEVEL.INFO,
                    "log_message": "info message",
                },),
            (
                {
                    "label": "warning_level",
                    "log_level": custom_types.LEVEL.WARNING,
                    "log_message": "warning message",
                },),
            (
                {
                    "label": "error_level",
                    "log_level": custom_types.LEVEL.ERROR,
                    "log_message": "error message",
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        fd_remotecontrol.RemoteControl.Client,
        "log_message",
        new_callable=mock.AsyncMock,
    )
    def test_send_log_command(
            self, parameterized_dict, mock_rcs_log_message) -> None:
        """Testcase for FuchsiaDevice._send_log_command()"""
        # pylint: disable=protected-access
        self.fd_obj._send_log_command(
            tag="test",
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"])

        mock_rcs_log_message.assert_called()

    @mock.patch.object(
        fd_remotecontrol.RemoteControl.Client,
        "log_message",
        new_callable=mock.AsyncMock,
    )
    def test_send_log_command_error(self, mock_rcs_log_message) -> None:
        """Testcase for FuchsiaDevice._send_log_command() when the log FIDL call
        raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_rcs_log_message.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            self.fd_obj._send_log_command(
                tag="test", level=custom_types.LEVEL.ERROR, message="test")

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    def test_send_reboot_command(self, mock_admin_reboot) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command()"""
        # pylint: disable=protected-access
        self.fd_obj._send_reboot_command()

        mock_admin_reboot.assert_called()

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    def test_send_reboot_command_error(self, mock_admin_reboot) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command() when the reboot
        FIDL call raises a non-ZX_ERR_PEER_CLOSED error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_admin_reboot.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            self.fd_obj._send_reboot_command()

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    def test_send_reboot_command_error_is_peer_closed(
            self, mock_admin_reboot) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command() when the reboot
        FIDL call raises a ZX_ERR_PEER_CLOSED error.  This error should not
        result in `FuchsiaControllerError` being raised."""
        mock_admin_reboot.side_effect = fuchsia_controller.ZxStatus(
            fuchsia_controller.ZxStatus.ZX_ERR_PEER_CLOSED)
        # pylint: disable=protected-access
        self.fd_obj._send_reboot_command()

        mock_admin_reboot.assert_called()

    def test_send_snapshot_command(self) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command()"""
        with self.assertRaises(NotImplementedError):
            # pylint: disable=protected-access
            self.fd_obj._send_snapshot_command()


if __name__ == "__main__":
    unittest.main()
