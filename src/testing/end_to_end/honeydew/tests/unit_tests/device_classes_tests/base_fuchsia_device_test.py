#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.device_classes.base_fuchsia_device.py."""

import base64
from typing import Any, Dict
import unittest
from unittest import mock

from parameterized import parameterized

from honeydew import custom_types
from honeydew import errors
from honeydew.device_classes import base_fuchsia_device
from honeydew.interfaces.device_classes import \
    fuchsia_device as fuchsia_device_interface
from honeydew.interfaces.device_classes import transports_capable

# pylint: disable=protected-access
# pytype: disable=attribute-error
_INPUT_ARGS: Dict[str, str] = {
    "device_name": "fuchsia-emulator",
    "ssh_private_key": "/tmp/.ssh/pkey",
    "ssh_user": "root",
}

_MOCK_ARGS: Dict[str, str] = {
    "device_type": "qemu-x64",
}

_BASE64_ENCODED_BYTES: bytes = base64.b64decode("some base64 encoded string==")


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class BaseFuchsiaDeviceTests(unittest.TestCase):
    """Unit tests for honeydew.device_classes.base_fuchsia_device.py."""

    @mock.patch(
        # pylint: disable=line-too-long
        "honeydew.device_classes.base_fuchsia_device.BaseFuchsiaDevice.__abstractmethods__",
        set())
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH,
        "check_connection",
        autospec=True)
    def setUp(
            self, mock_ssh_check_connection, mock_ffx_check_connection) -> None:
        super().setUp()

        # pylint: disable=abstract-class-instantiated
        # pylint: disable=line-too-long
        self.fd_obj = base_fuchsia_device.BaseFuchsiaDevice(
            device_name=_INPUT_ARGS["device_name"],
            ssh_private_key=_INPUT_ARGS["ssh_private_key"]
        )  # type: ignore[abstract]

        mock_ffx_check_connection.assert_called()
        mock_ssh_check_connection.assert_called()

    # # List all the tests related to __init__ in alphabetical order
    @parameterized.expand(
        [
            (
                {
                    "label": "all_optional_params",
                    "mandatory_params":
                        {
                            "device_name": _INPUT_ARGS["device_name"],
                            "ssh_private_key": _INPUT_ARGS["ssh_private_key"],
                        },
                    "optional_params": {
                        "ssh_user": _INPUT_ARGS["ssh_user"],
                    },
                },),
            (
                {
                    "label": "no_optional_params",
                    "mandatory_params":
                        {
                            "device_name": _INPUT_ARGS["device_name"],
                            "ssh_private_key": _INPUT_ARGS["ssh_private_key"],
                        },
                    "optional_params": {},
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch(
        # pylint: disable=line-too-long
        "honeydew.device_classes.base_fuchsia_device.BaseFuchsiaDevice.__abstractmethods__",
        set())
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "check_connection",
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH,
        "check_connection",
        autospec=True)
    def test_fuchsia_device_init(
            self, parameterized_dict, mock_ssh_check_connection,
            mock_ffx_check_connection) -> None:
        """Verify FuchsiaDevice class instantiation"""
        optional_params: Dict[str, Any] = parameterized_dict["optional_params"]

        device_name: str = parameterized_dict["mandatory_params"]["device_name"]
        ssh_private_key: str = parameterized_dict["mandatory_params"][
            "ssh_private_key"]

        # pylint: disable=abstract-class-instantiated
        _ = base_fuchsia_device.BaseFuchsiaDevice(
            device_name=device_name,
            ssh_private_key=ssh_private_key,
            **optional_params)  # type: ignore[abstract]

        mock_ffx_check_connection.assert_called()
        mock_ssh_check_connection.assert_called()

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice)

    # List all the tests related to static properties in alphabetical order
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "get_target_type",
        return_value=_MOCK_ARGS["device_type"],
        autospec=True)
    def test_device_type(self, mock_ffx_get_target_type) -> None:
        """Testcase for BaseFuchsiaDevice.device_type property"""
        self.assertEqual(self.fd_obj.device_type, _MOCK_ARGS["device_type"])
        mock_ffx_get_target_type.assert_called()

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_product_info",
        return_value={
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
        new_callable=mock.PropertyMock)
    def test_manufacturer(self, *unused_args) -> None:
        """Testcase for BaseFuchsiaDevice.manufacturer property"""
        self.assertEqual(self.fd_obj.manufacturer, "default-manufacturer")

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_product_info",
        return_value={
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
        new_callable=mock.PropertyMock)
    def test_model(self, *unused_args) -> None:
        """Testcase for BaseFuchsiaDevice.model property"""
        self.assertEqual(self.fd_obj.model, "default-model")

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_product_info",
        return_value={
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
        new_callable=mock.PropertyMock)
    def test_product_name(self, *unused_args) -> None:
        """Testcase for BaseFuchsiaDevice.product_name property"""
        self.assertEqual(self.fd_obj.product_name, "default-product-name")

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_device_info",
        return_value={
            "serial_number": "default-serial-number",
        },
        new_callable=mock.PropertyMock)
    def test_serial_number(self, *unused_args) -> None:
        """Testcase for BaseFuchsiaDevice.serial_number property"""
        self.assertEqual(self.fd_obj.serial_number, "default-serial-number")

    # # List all the tests related to dynamic properties in alphabetical order
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_build_info",
        return_value={
            "version": "1.2.3",
        },
        new_callable=mock.PropertyMock)
    def test_firmware_version(self, *unused_args) -> None:
        """Testcase for BaseFuchsiaDevice.firmware_version property"""
        self.assertEqual(self.fd_obj.firmware_version, "1.2.3")

    # # List all the tests related to transports in alphabetical order
    def test_fuchsia_device_is_fastboot_capable(self) -> None:
        """Test case to make sure fuchsia device is Fastboot capable"""
        self.assertIsInstance(
            self.fd_obj, transports_capable.FastbootCapableDevice)

    def test_fuchsia_device_is_ffx_capable(self) -> None:
        """Test case to make sure fuchsia device is FFX capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.FFXCapableDevice)

    def test_fuchsia_device_is_ssh_capable(self) -> None:
        """Test case to make sure fuchsia device is SSH capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.SSHCapableDevice)

    # List all the tests related to public methods in alphabetical order
    def test_close(self) -> None:
        """Testcase for BaseFuchsiaDevice.close()"""
        self.fd_obj.close()

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
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_log_command",
        autospec=True)
    def test_log_message_to_device(
            self, parameterized_dict, mock_send_log_command) -> None:
        """Testcase for BaseFuchsiaDevice.log_message_to_device()"""
        self.fd_obj.log_message_to_device(
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"])

        mock_send_log_command.assert_called_with(
            self.fd_obj,
            tag="lacewing",
            message=mock.ANY,
            level=parameterized_dict["log_level"])

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[False, True],
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice, "on_device_boot", autospec=True)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_log_command",
        autospec=True)
    def test_power_cycle(
            self, mock_send_log_command, mock_on_device_boot,
            mock_is_target_connected) -> None:
        """Testcase for BaseFuchsiaDevice.power_cycle()"""
        power_switch = mock.MagicMock(
            spec=base_fuchsia_device.power_switch_interface.PowerSwitch)
        self.fd_obj.power_cycle(power_switch=power_switch, outlet=5)

        self.assertEqual(mock_send_log_command.call_count, 2)
        self.assertEqual(mock_is_target_connected.call_count, 2)
        power_switch.power_off.assert_called()
        power_switch.power_on.assert_called()
        mock_on_device_boot.assert_called()

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[False, True],
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice, "on_device_boot", autospec=True)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_log_command",
        autospec=True)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_reboot_command",
        autospec=True)
    def test_reboot(
            self, mock_send_reboot_command, mock_send_log_command,
            mock_on_device_boot, mock_is_target_connected) -> None:
        """Testcase for BaseFuchsiaDevice.reboot()"""
        self.fd_obj.reboot()

        self.assertEqual(mock_send_log_command.call_count, 2)
        self.assertEqual(mock_is_target_connected.call_count, 2)
        mock_send_reboot_command.assert_called()
        mock_on_device_boot.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "no_snapshot_file_arg",
                    "directory": "/tmp",
                    "optional_params": {},
                },),
            (
                {
                    "label": "snapshot_file_arg",
                    "directory": "/tmp",
                    "optional_params": {
                        "snapshot_file": "snapshot.zip",
                    },
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_snapshot_command",
        return_value=_BASE64_ENCODED_BYTES,
        autospec=True)
    @mock.patch.object(base_fuchsia_device.os, "makedirs", autospec=True)
    def test_snapshot(
            self, parameterized_dict, mock_makedirs,
            mock_send_snapshot_command) -> None:
        """Testcase for BaseFuchsiaDevice.snapshot()"""
        directory: str = parameterized_dict["directory"]
        optional_params: Dict[str, Any] = parameterized_dict["optional_params"]

        with mock.patch("builtins.open", mock.mock_open()) as mocked_file:
            snapshot_file_path: str = self.fd_obj.snapshot(
                directory=directory, **optional_params)

        if "snapshot_file" in optional_params:
            self.assertEqual(
                snapshot_file_path,
                f"{directory}/{optional_params['snapshot_file']}")
        else:
            self.assertRegex(
                snapshot_file_path,
                f"{directory}/Snapshot_{self.fd_obj.device_name}_.*.zip")

        mocked_file.assert_called()
        mocked_file().write.assert_called()
        mock_makedirs.assert_called()
        mock_send_snapshot_command.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[True, False],
        autospec=True)
    def test_wait_for_offline_success(
            self, mock_ffx_is_target_connected, mock_sleep) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_offline() success case"""
        self.fd_obj.wait_for_offline()

        mock_ffx_is_target_connected.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        return_value=True,
        autospec=True)
    def test_wait_for_offline_fail(self, mock_ffx_is_target_connected) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_offline() failure case"""
        with self.assertRaisesRegex(errors.FuchsiaDeviceError,
                                    "failed to go offline"):
            self.fd_obj.wait_for_offline(timeout=2)

        mock_ffx_is_target_connected.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[False, True],
        autospec=True)
    def test_wait_for_online_success(
            self, mock_ffx_is_target_connected, mock_sleep) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_online() success case"""
        self.fd_obj.wait_for_online()

        mock_ffx_is_target_connected.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        return_value=False,
        autospec=True)
    def test_wait_for_online_fail(self, mock_ffx_is_target_connected) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_online() failure case"""
        with self.assertRaisesRegex(errors.FuchsiaDeviceError,
                                    "failed to go online"):
            self.fd_obj.wait_for_online(timeout=2)

        mock_ffx_is_target_connected.assert_called()


if __name__ == "__main__":
    unittest.main()
