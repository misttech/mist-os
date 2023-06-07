#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.device_classes.sl4f.fuchsia_device.py."""

from typing import Any, Dict
import unittest
from unittest import mock

from honeydew import custom_types
from honeydew import errors
from honeydew.device_classes.sl4f import fuchsia_device
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import \
    fuchsia_device as fuchsia_device_interface
from honeydew.interfaces.device_classes import transports_capable
from parameterized import parameterized

# pylint: disable=protected-access
# pytype: disable=attribute-error
_INPUT_ARGS: Dict[str, Any] = {
    "device_name": "fuchsia-emulator",
    "ssh_private_key": "/tmp/.ssh/pkey",
    "ssh_user": "root",
}

_MOCK_ARGS: Dict[str, Any] = {
    "device_type": "qemu-x64",
}

_BASE64_ENCODED_STR = "some base64 encoded string=="


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FuchsiaDeviceSL4FTests(unittest.TestCase):
    """Unit tests for honeydew.device_classes.sl4f.fuchsia_device.py."""

    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True)
    @mock.patch.object(
        fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True)
    def setUp(
            self, mock_ssh_check_connection, mock_sl4f_start_server,
            mock_sl4f_check_connection, mock_ffx_check_connection) -> None:
        super().setUp()

        self.fd_obj = fuchsia_device.FuchsiaDevice(
            device_name=_INPUT_ARGS["device_name"],
            ssh_private_key=_INPUT_ARGS["ssh_private_key"])

        mock_ssh_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    # List all the tests related to __init__ in alphabetical order
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
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True)
    @mock.patch.object(
        fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True)
    def test_fuchsia_device_init(
            self, parameterized_dict, mock_ssh_check_connection,
            mock_sl4f_start_server, mock_sl4f_check_connection,
            mock_ffx_check_connection) -> None:
        """Verify FuchsiaDevice class instantiation"""
        optional_params: Dict[str, Any] = parameterized_dict["optional_params"]

        device_name: str = parameterized_dict["mandatory_params"]["device_name"]
        ssh_private_key: str = parameterized_dict["mandatory_params"][
            "ssh_private_key"]

        fd_obj = fuchsia_device.FuchsiaDevice(
            device_name=device_name,
            ssh_private_key=ssh_private_key,
            **optional_params)

        self.assertIsInstance(fd_obj, fuchsia_device.FuchsiaDevice)

        mock_ssh_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice)

    # List all the tests related to affordances in alphabetical order
    def test_fuchsia_device_is_bluetooth_capable(self) -> None:
        """Test case to make sure fuchsia device is bluetooth capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.BluetoothCapableDevice)

    def test_fuchsia_device_is_component_capable(self) -> None:
        """Test case to make sure fuchsia device is component capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.ComponentCapableDevice)

    def test_fuchsia_device_is_tracing_capable(self) -> None:
        """Test case to make sure fuchsia device is tracing capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.TracingCapableDevice)

    # List all the tests related to transports in alphabetical order
    def test_fuchsia_device_is_ssh_capable(self) -> None:
        """Test case to make sure fuchsia device is SSH capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.SSHCapableDevice)

    def test_fuchsia_device_is_ffx_capable(self) -> None:
        """Test case to make sure fuchsia device is FFX capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.FFXCapableDevice)

    def test_fuchsia_device_is_sl4f_capable(self) -> None:
        """Test case to make sure fuchsia device is sl4f capable"""
        self.assertIsInstance(self.fd_obj, transports_capable.SL4FCapableDevice)

    # List all the tests related to static properties in alphabetical order
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "get_target_type",
        return_value=_MOCK_ARGS["device_type"],
        autospec=True)
    def test_device_type(self, mock_ffx_get_target_type) -> None:
        """Testcase for FuchsiaDevice.device_type property"""
        self.assertEqual(self.fd_obj.device_type, _MOCK_ARGS["device_type"])
        mock_ffx_get_target_type.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_manufacturer(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.manufacturer property"""
        self.assertEqual(self.fd_obj.manufacturer, "default-manufacturer")

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_model(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.model property"""
        self.assertEqual(self.fd_obj.model, "default-model")

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={
            "result":
                {
                    "manufacturer": "default-manufacturer",
                    "model": "default-model",
                    "name": "default-product-name",
                }
        },
        autospec=True)
    def test_product_name(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.product_name property"""
        self.assertEqual(self.fd_obj.product_name, "default-product-name")

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": {
            "serial_number": "default-serial-number",
        }},
        autospec=True)
    def test_serial_number(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.serial_number property"""
        self.assertEqual(self.fd_obj.serial_number, "default-serial-number")

        mock_sl4f_run.assert_called()

    # List all the tests related to dynamic properties in alphabetical order
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": "1.2.3"},
        autospec=True)
    def test_firmware_version(self, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.firmware_version property"""
        self.assertEqual(self.fd_obj.firmware_version, "1.2.3")

        mock_sl4f_run.assert_called()

    # List all the tests related to public methods in alphabetical order

    # pytype: disable=attribute-error
    def test_close(self) -> None:
        """Testcase for FuchsiaDevice.close()"""
        self.fd_obj.close()

    # pytype: enable=attribute-error

    @parameterized.expand(
        [
            (
                {
                    "label": "info_level",
                    "log_level": custom_types.LEVEL.INFO,
                    "sl4f_method": "LogInfo",
                    "log_message": "info message",
                },),
            (
                {
                    "label": "warning_level",
                    "log_level": custom_types.LEVEL.WARNING,
                    "sl4f_method": "LogWarning",
                    "log_message": "warning message",
                },),
            (
                {
                    "label": "error_level",
                    "log_level": custom_types.LEVEL.ERROR,
                    "sl4f_method": "LogError",
                    "log_message": "error message",
                },),
        ],
        name_func=_custom_test_name_func)
    @mock.patch.object(fuchsia_device.sl4f_transport.SL4F, "run", autospec=True)
    def test_log_message_to_device(
            self, parameterized_dict, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.log_message_to_device()"""
        self.fd_obj.log_message_to_device(
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"])

        mock_sl4f_run.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_wait_for_bootup_complete",
        autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "_wait_for_offline", autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "log_message_to_device", autospec=True)
    def test_power_cycle(
            self, mock_log_message_to_device, mock_wait_for_offline,
            mock_wait_for_bootup_complete) -> None:
        """Testcase for FuchsiaDevice.power_cycle()"""
        power_switch = mock.MagicMock(
            spec=fuchsia_device.power_switch_interface.PowerSwitch)
        self.fd_obj.power_cycle(power_switch=power_switch, outlet=5)

        self.assertEqual(mock_log_message_to_device.call_count, 2)

        power_switch.power_off.assert_called()
        mock_wait_for_offline.assert_called()

        power_switch.power_on.assert_called()
        mock_wait_for_bootup_complete.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_wait_for_bootup_complete",
        autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "_wait_for_offline", autospec=True)
    @mock.patch.object(fuchsia_device.sl4f_transport.SL4F, "run", autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "log_message_to_device", autospec=True)
    def test_reboot(
            self, mock_log_message_to_device, mock_sl4f_run,
            mock_wait_for_offline, mock_wait_for_bootup_complete) -> None:
        """Testcase for FuchsiaDevice.reboot()"""
        self.fd_obj.reboot()

        self.assertEqual(mock_log_message_to_device.call_count, 2)
        mock_sl4f_run.assert_called()
        mock_wait_for_offline.assert_called()
        mock_wait_for_bootup_complete.assert_called()

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
        fuchsia_device.sl4f_transport.SL4F,
        "run",
        return_value={"result": {
            "zip": _BASE64_ENCODED_STR
        }},
        autospec=True)
    @mock.patch.object(fuchsia_device.os, "makedirs", autospec=True)
    def test_snapshot(
            self, parameterized_dict, mock_makedirs, mock_sl4f_run) -> None:
        """Testcase for FuchsiaDevice.snapshot()"""
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
        mock_sl4f_run.assert_called()

    # List all the tests related to private methods in alphabetical order

    # Note - Test for FuchsiaDevice._product_info has been covered in
    # persistent properties

    @mock.patch.object(
        fuchsia_device.bluetooth_sl4f.Bluetooth, "sys_init", autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "device_type",
        new_callable=mock.PropertyMock,
        return_value="x64")
    @mock.patch.object(
        fuchsia_device.sl4f_transport.SL4F, "start_server", autospec=True)
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "_wait_for_online", autospec=True)
    def test_wait_for_bootup_complete(
            self, mock_wait_for_online, mock_ssh_check_connection,
            mock_ffx_check_connection, mock_sl4f_start_server, mock_device_type,
            mock_bluetooth_sys_init) -> None:
        """Testcase for FuchsiaDevice._wait_for_bootup_complete()"""
        self.fd_obj._wait_for_bootup_complete(timeout=10)

        mock_wait_for_online.assert_called()
        mock_ssh_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_device_type.assert_called()
        mock_bluetooth_sys_init.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[True, False],
        autospec=True)
    def test_wait_for_offline_success(
            self, mock_ffx_is_target_connected, mock_sleep) -> None:
        """Testcase for FuchsiaDevice._wait_for_offline() success case"""
        self.fd_obj._wait_for_offline()

        mock_ffx_is_target_connected.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        return_value=True,
        autospec=True)
    def test_wait_for_offline_fail(self, mock_ffx_is_target_connected) -> None:
        """Testcase for FuchsiaDevice._wait_for_offline() failure case"""
        with self.assertRaisesRegex(errors.FuchsiaDeviceError,
                                    "failed to go offline"):
            self.fd_obj._wait_for_offline(timeout=2)

        mock_ffx_is_target_connected.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        side_effect=[False, True],
        autospec=True)
    def test_wait_for_online_success(
            self, mock_ffx_is_target_connected, mock_sleep) -> None:
        """Testcase for FuchsiaDevice._wait_for_online() success case"""
        self.fd_obj._wait_for_online()

        mock_ffx_is_target_connected.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(
        fuchsia_device.ffx_transport.FFX,
        "is_target_connected",
        return_value=False,
        autospec=True)
    def test_wait_for_online_fail(self, mock_ffx_is_target_connected) -> None:
        """Testcase for FuchsiaDevice._wait_for_online() failure case"""
        with self.assertRaisesRegex(errors.FuchsiaDeviceError,
                                    "failed to go online"):
            self.fd_obj._wait_for_online(timeout=2)

        mock_ffx_is_target_connected.assert_called()


if __name__ == "__main__":
    unittest.main()
