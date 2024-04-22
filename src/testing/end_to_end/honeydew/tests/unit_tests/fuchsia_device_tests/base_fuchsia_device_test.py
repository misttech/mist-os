#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.fuchsia_device.base_fuchsia_device.py."""

import base64
import os
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller
from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.ffx import session as session_ffx
from honeydew.affordances.ffx.ui import screenshot as screenshot_ffx
from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.interfaces.auxiliary_devices import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import fastboot as fastboot_transport
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.typing import custom_types

# pylint: disable=protected-access
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

_MOCK_ARGS: dict[str, str] = {"board": "x64", "product": "core"}

_BASE64_ENCODED_BYTES: bytes = base64.b64decode("some base64 encoded string==")


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class BaseFuchsiaDeviceTests(unittest.TestCase):
    """Unit tests for honeydew.fuchsia_device.base_fuchsia_device.py."""

    def setUp(self) -> None:
        super().setUp()

        with (
            mock.patch.object(
                ssh_transport.SSH,
                "check_connection",
                autospec=True,
            ) as mock_ssh_check_connection,
            mock.patch.object(
                ffx_transport.FFX,
                "check_connection",
                autospec=True,
            ) as mock_ffx_check_connection,
            mock.patch(
                # pylint: disable=line-too-long
                "honeydew.fuchsia_device.base_fuchsia_device.BaseFuchsiaDevice.__abstractmethods__",
                set(),
            ),
        ):
            # pylint: disable=abstract-class-instantiated
            self.fd_obj = base_fuchsia_device.BaseFuchsiaDevice(
                device_name=_INPUT_ARGS["device_name"],
                ssh_private_key=_INPUT_ARGS["ssh_private_key"],
                ffx_config=_INPUT_ARGS["ffx_config"],
            )  # type: ignore[abstract]

            mock_ffx_check_connection.assert_called()
            mock_ssh_check_connection.assert_called()

    # # List all the tests related to __init__
    @parameterized.expand(  # type: ignore[misc]
        [
            (
                {
                    "label": "all_optional_params",
                    "mandatory_params": {
                        "device_name": _INPUT_ARGS["device_name"],
                        "ssh_private_key": _INPUT_ARGS["ssh_private_key"],
                    },
                    "optional_params": {
                        "ssh_user": _INPUT_ARGS["ssh_user"],
                    },
                },
            ),
            (
                {
                    "label": "no_optional_params",
                    "mandatory_params": {
                        "device_name": _INPUT_ARGS["device_name"],
                        "ssh_private_key": _INPUT_ARGS["ssh_private_key"],
                    },
                    "optional_params": {},
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch(
        # pylint: disable=line-too-long
        "honeydew.fuchsia_device.base_fuchsia_device.BaseFuchsiaDevice.__abstractmethods__",
        set(),
    )
    @mock.patch.object(ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(ssh_transport.SSH, "check_connection", autospec=True)
    def test_fuchsia_device_init(
        self,
        parameterized_dict: dict[str, Any],
        mock_ssh_check_connection: mock.Mock,
        mock_ffx_check_connection: mock.Mock,
    ) -> None:
        """Verify FuchsiaDevice class instantiation"""
        optional_params: dict[str, Any] = parameterized_dict["optional_params"]

        device_name: str = parameterized_dict["mandatory_params"]["device_name"]
        ssh_private_key: str = parameterized_dict["mandatory_params"][
            "ssh_private_key"
        ]

        # pylint: disable=abstract-class-instantiated
        _ = base_fuchsia_device.BaseFuchsiaDevice(
            device_name=device_name,
            ssh_private_key=ssh_private_key,
            ffx_config=_INPUT_ARGS["ffx_config"],
            **optional_params,
        )  # type: ignore[abstract]

        mock_ffx_check_connection.assert_called()
        mock_ssh_check_connection.assert_called()

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice
        )

    # List all the tests related to transports
    @mock.patch.object(
        fastboot_transport.Fastboot,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_fastboot_transport(self, mock_fastboot_init: mock.Mock) -> None:
        """Test case to make sure base_fuchsia_device supports fastboot
        transport."""
        self.assertIsInstance(
            self.fd_obj.fastboot,
            fastboot_transport.Fastboot,
        )
        mock_fastboot_init.assert_called_once()

    def test_ffx_transport(self) -> None:
        """Test case to make sure base_fuchsia_device supports ffx transport."""
        self.assertIsInstance(
            self.fd_obj.ffx,
            ffx_transport.FFX,
        )

    def test_ssh_transport(self) -> None:
        """Test case to make sure base_fuchsia_device supports ssh transport."""
        self.assertIsInstance(
            self.fd_obj.ssh,
            ssh_transport.SSH,
        )

    def test_sl4f_transport(self) -> None:
        """Test case to make sure base_fuchsia_device does not support sl4f
        transport."""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.sl4f  # pylint: disable=pointless-statement

    def test_fuchsia_controller_transport(self) -> None:
        """Test case to make sure base_fuchsia_device does not support
        fuchsia-controller transport."""
        with self.assertRaises(NotImplementedError):
            self.fd_obj.fuchsia_controller  # pylint: disable=pointless-statement

    # List all the tests related to affordances
    def test_session(self) -> None:
        """Test case to make sure base_fuchsia_device supports session
        affordance implemented using FFX"""
        self.assertIsInstance(self.fd_obj.session, session_ffx.Session)

    def test_screenshot(self) -> None:
        """Test case to make sure base_fuchsia_device supports screenshot
        affordance implemented using FFX"""
        self.assertIsInstance(
            self.fd_obj.screenshot,
            screenshot_ffx.Screenshot,
        )

    # List all the tests related to static properties
    @mock.patch.object(
        ffx_transport.FFX,
        "get_target_board",
        return_value=_MOCK_ARGS["board"],
        autospec=True,
    )
    def test_board(self, mock_ffx_get_target_board: mock.Mock) -> None:
        """Testcase for BaseFuchsiaDevice.board property"""
        self.assertEqual(self.fd_obj.board, _MOCK_ARGS["board"])
        mock_ffx_get_target_board.assert_called()

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_product_info",
        return_value={
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
        new_callable=mock.PropertyMock,
    )
    def test_manufacturer(self, *unused_args: Any) -> None:
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
        new_callable=mock.PropertyMock,
    )
    def test_model(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.model property"""
        self.assertEqual(self.fd_obj.model, "default-model")

    @mock.patch.object(
        ffx_transport.FFX,
        "get_target_product",
        return_value=_MOCK_ARGS["product"],
        autospec=True,
    )
    def test_product(self, mock_ffx_get_target_product: mock.Mock) -> None:
        """Testcase for BaseFuchsiaDevice.product property"""
        self.assertEqual(self.fd_obj.product, _MOCK_ARGS["product"])
        mock_ffx_get_target_product.assert_called()

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_product_info",
        return_value={
            "manufacturer": "default-manufacturer",
            "model": "default-model",
            "name": "default-product-name",
        },
        new_callable=mock.PropertyMock,
    )
    def test_product_name(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.product_name property"""
        self.assertEqual(self.fd_obj.product_name, "default-product-name")

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_device_info",
        return_value={
            "serial_number": "default-serial-number",
        },
        new_callable=mock.PropertyMock,
    )
    def test_serial_number(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.serial_number property"""
        self.assertEqual(self.fd_obj.serial_number, "default-serial-number")

    # List all the tests related to dynamic properties
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_build_info",
        return_value={
            "version": "1.2.3",
        },
        new_callable=mock.PropertyMock,
    )
    def test_firmware_version(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.firmware_version property"""
        self.assertEqual(self.fd_obj.firmware_version, "1.2.3")

    # List all the tests related to affordances
    def test_fuchsia_device_is_reboot_capable(self) -> None:
        """Test case to make sure fuchsia device is reboot capable"""
        self.assertIsInstance(
            self.fd_obj, affordances_capable.RebootCapableDevice
        )

    # List all the tests related to public methods
    @mock.patch.object(
        ffx_transport.FFX,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        ssh_transport.SSH,
        "check_connection",
        autospec=True,
    )
    def test_health_check(
        self,
        mock_ssh_check_connection: mock.Mock,
        mock_ffx_check_connection: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.health_check()"""
        self.fd_obj.health_check()
        mock_ssh_check_connection.assert_called_once_with(self.fd_obj.ssh)
        mock_ffx_check_connection.assert_called_once_with(self.fd_obj.ffx)

    @parameterized.expand(  # type: ignore[misc]
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
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_log_command",
        autospec=True,
    )
    def test_log_message_to_device(
        self,
        parameterized_dict: dict[str, Any],
        mock_send_log_command: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.log_message_to_device()"""
        self.fd_obj.log_message_to_device(
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"],
        )

        mock_send_log_command.assert_called_with(
            self.fd_obj,
            tag="lacewing",
            message=mock.ANY,
            level=parameterized_dict["log_level"],
        )

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                {
                    "label": "no_register_for_on_device_boot",
                    "register_for_on_device_boot": None,
                    "expected_exception": False,
                },
            ),
            (
                {
                    "label": "register_for_on_device_boot_fn_returning_success",
                    "register_for_on_device_boot": lambda: None,
                    "expected_exception": False,
                },
            ),
            (
                {
                    "label": "register_for_on_device_boot_fn_returning_exception",
                    "register_for_on_device_boot": lambda: 1 / 0,
                    "expected_exception": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_on_device_boot(self, parameterized_dict: dict[str, Any]) -> None:
        """Testcase for BaseFuchsiaDevice.on_device_boot()"""
        # Reset the `_on_device_boot_fns` variable at the beginning of the test
        self.fd_obj._on_device_boot_fns = []

        if parameterized_dict["register_for_on_device_boot"]:
            self.fd_obj.register_for_on_device_boot(
                parameterized_dict["register_for_on_device_boot"]
            )
        if parameterized_dict["expected_exception"]:
            with self.assertRaises(Exception):
                self.fd_obj.on_device_boot()
        else:
            self.fd_obj.on_device_boot()

        # Reset the `_on_device_boot_fns` variable at the end of the test
        self.fd_obj._on_device_boot_fns = []

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice, "on_device_boot", autospec=True
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "wait_for_online",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "wait_for_offline",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "log_message_to_device",
        autospec=True,
    )
    def test_power_cycle(
        self,
        mock_log_message_to_device: mock.Mock,
        mock_wait_for_offline: mock.Mock,
        mock_wait_for_online: mock.Mock,
        mock_on_device_boot: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.power_cycle()"""
        power_switch = mock.MagicMock(spec=power_switch_interface.PowerSwitch)
        self.fd_obj.power_cycle(power_switch=power_switch, outlet=5)

        self.assertEqual(mock_log_message_to_device.call_count, 2)
        mock_wait_for_offline.assert_called()
        mock_wait_for_online.assert_called()
        mock_on_device_boot.assert_called()

    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice, "on_device_boot", autospec=True
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "wait_for_online",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "wait_for_offline",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_reboot_command",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "log_message_to_device",
        autospec=True,
    )
    def test_reboot(
        self,
        mock_log_message_to_device: mock.Mock,
        mock_send_reboot_command: mock.Mock,
        mock_wait_for_offline: mock.Mock,
        mock_wait_for_online: mock.Mock,
        mock_on_device_boot: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.reboot()"""
        self.fd_obj.reboot()

        self.assertEqual(mock_log_message_to_device.call_count, 2)
        mock_send_reboot_command.assert_called()
        mock_wait_for_offline.assert_called()
        mock_wait_for_online.assert_called()
        mock_on_device_boot.assert_called()

    def test_register_for_on_device_boot(self) -> None:
        """Testcase for BaseFuchsiaDevice.register_for_on_device_boot()"""
        self.fd_obj.register_for_on_device_boot(fn=lambda: None)

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                {
                    "label": "no_snapshot_file_arg",
                    "directory": "/tmp",
                    "optional_params": {},
                },
            ),
            (
                {
                    "label": "snapshot_file_arg",
                    "directory": "/tmp",
                    "optional_params": {
                        "snapshot_file": "snapshot.zip",
                    },
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        base_fuchsia_device.BaseFuchsiaDevice,
        "_send_snapshot_command",
        return_value=_BASE64_ENCODED_BYTES,
        autospec=True,
    )
    @mock.patch.object(os, "makedirs", autospec=True)
    def test_snapshot(
        self,
        parameterized_dict: dict[str, Any],
        mock_makedirs: mock.Mock,
        mock_send_snapshot_command: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.snapshot()"""
        directory: str = parameterized_dict["directory"]
        optional_params: dict[str, Any] = parameterized_dict["optional_params"]

        with mock.patch("builtins.open", mock.mock_open()) as mocked_file:
            snapshot_file_path: str = self.fd_obj.snapshot(
                directory=directory, **optional_params
            )

        if "snapshot_file" in optional_params:
            self.assertEqual(
                snapshot_file_path,
                f"{directory}/{optional_params['snapshot_file']}",
            )
        else:
            self.assertRegex(
                snapshot_file_path,
                f"{directory}/Snapshot_{self.fd_obj.device_name}_.*.zip",
            )

        mocked_file.assert_called()
        mocked_file().write.assert_called()
        mock_makedirs.assert_called()
        mock_send_snapshot_command.assert_called()

    @mock.patch.object(
        ffx_transport.FFX,
        "wait_for_rcs_disconnection",
        autospec=True,
    )
    def test_wait_for_offline_success(
        self, mock_ffx_wait_for_rcs_disconnection: mock.Mock
    ) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_offline() success case"""
        self.fd_obj.wait_for_offline()

        mock_ffx_wait_for_rcs_disconnection.assert_called()

    @mock.patch.object(
        ffx_transport.FFX,
        "wait_for_rcs_disconnection",
        side_effect=errors.FfxCommandError("error"),
        autospec=True,
    )
    def test_wait_for_offline_fail(
        self, mock_ffx_wait_for_rcs_disconnection: mock.Mock
    ) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_offline() failure case"""
        with self.assertRaisesRegex(
            errors.FuchsiaDeviceError, "failed to go offline"
        ):
            self.fd_obj.wait_for_offline(timeout=2)

        mock_ffx_wait_for_rcs_disconnection.assert_called()

    @mock.patch.object(
        ffx_transport.FFX,
        "wait_for_rcs_connection",
        autospec=True,
    )
    def test_wait_for_online_success(
        self, mock_ffx_wait_for_rcs_connection: mock.Mock
    ) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_online() success case"""
        self.fd_obj.wait_for_online()

        mock_ffx_wait_for_rcs_connection.assert_called()

    @mock.patch.object(
        ffx_transport.FFX,
        "wait_for_rcs_connection",
        side_effect=errors.FuchsiaDeviceError("some error"),
        autospec=True,
    )
    def test_wait_for_online_fail(
        self, mock_ffx_wait_for_rcs_connection: mock.Mock
    ) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_online() failure case"""
        with self.assertRaisesRegex(
            errors.FuchsiaDeviceError, "failed to go online"
        ):
            self.fd_obj.wait_for_online()

        mock_ffx_wait_for_rcs_connection.assert_called()


if __name__ == "__main__":
    unittest.main()
