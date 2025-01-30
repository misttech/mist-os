# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.fuchsia_device.fuchsia_device.py."""

import base64
import os
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

import fidl.fuchsia_buildinfo as f_buildinfo
import fidl.fuchsia_developer_remotecontrol as fd_remotecontrol
import fidl.fuchsia_feedback as f_feedback
import fidl.fuchsia_hardware_power_statecontrol as fhp_statecontrol
import fidl.fuchsia_hwinfo as f_hwinfo
import fidl.fuchsia_io as f_io
import fuchsia_controller_py as fuchsia_controller
from fuchsia_controller_py import ZxStatus
from parameterized import param, parameterized

from honeydew import errors
from honeydew.affordances.connectivity.wlan.wlan import wlan_using_fc
from honeydew.affordances.connectivity.wlan.wlan_policy import (
    wlan_policy_using_fc,
)
from honeydew.affordances.ffx import session as session_ffx
from honeydew.affordances.ffx.ui import screenshot as screenshot_ffx
from honeydew.affordances.fuchsia_controller import rtc as rtc_fc
from honeydew.affordances.fuchsia_controller import tracing as tracing_fc
from honeydew.affordances.fuchsia_controller.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_fc,
)
from honeydew.affordances.fuchsia_controller.ui import (
    user_input as user_input_fc,
)
from honeydew.affordances.power.system_power_state_controller import (
    system_power_state_controller_using_starnix,
)
from honeydew.affordances.sl4f.bluetooth import bluetooth_common
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_sl4f,
)
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_sl4f,
)
from honeydew.fuchsia_device import fuchsia_device
from honeydew.interfaces.auxiliary_devices import (
    power_switch as power_switch_interface,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.interfaces.transports import serial as serial_interface
from honeydew.transports import fastboot as fastboot_transport
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)
from honeydew.transports import serial_using_unix_socket
from honeydew.transports import sl4f as sl4f_transport
from honeydew.typing import custom_types

# pylint: disable=protected-access

_INPUT_ARGS: dict[str, Any] = {
    "device_name": "fuchsia-emulator",
    "device_serial_socket": "/tmp/socket",
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
        logs_dir="/tmp/logs",
        binary_path="/bin/ffx",
        logs_level="debug",
        mdns_enabled=False,
        subtools_search_path=None,
        proxy_timeout_secs=None,
        ssh_keepalive_timeout=None,
    ),
}


_MOCK_ARGS: dict[str, str] = {"board": "x64", "product": "core"}

_BASE64_ENCODED_BYTES: bytes = base64.b64decode("some base64 encoded string==")

_MOCK_DEVICE_PROPERTIES: dict[str, dict[str, Any]] = {
    "build_info": {
        "version": "123456",
    },
    "device_info_from_fidl": {
        "serial_number": "123456",
    },
    "product_info": {
        "manufacturer": "default-manufacturer",
        "model": "default-model",
        "name": "default-product-name",
    },
}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom test name function method."""
    test_func_name: str = testcase_func.__name__
    test_label: str

    try:
        params_dict: dict[str, Any] = param_arg.args[0]
        test_label = parameterized.to_safe_name(params_dict["label"])
    except Exception:  # pylint: disable=broad-except
        test_label = parameterized.to_safe_name(param_arg.kwargs["label"])

    return f"{test_func_name}_with_{test_label}"


def _file_read_result(data: f_io.Transfer) -> f_io.ReadableReadResult:
    ret = f_io.ReadableReadResult()
    ret.response = f_io.ReadableReadResponse(data=data)
    return ret


def _file_attr_resp(status: ZxStatus, size: int) -> f_io.NodeGetAttrResponse:
    return f_io.NodeGetAttrResponse(
        s=status.raw(),
        attributes=f_io.NodeAttributes(
            content_size=size,
            # The args below are arbitrary.
            mode=0,
            id=0,
            storage_size=0,
            link_count=0,
            creation_time=0,
            modification_time=0,
        ),
    )


class FuchsiaDeviceFCTests(unittest.TestCase):
    """Unit tests for honeydew.fuchsia_device.fuchsia_device.py."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self.fd_fc_obj: fuchsia_device.FuchsiaDevice
        super().__init__(*args, **kwargs)

    def setUp(self) -> None:
        with (
            mock.patch.object(
                fuchsia_controller_transport.FuchsiaController,
                "create_context",
                autospec=True,
            ) as mock_fc_create_context,
            mock.patch.object(
                ffx_transport.FFX,
                "check_connection",
                autospec=True,
            ) as mock_ffx_check_connection,
            mock.patch.object(
                fuchsia_controller_transport.FuchsiaController,
                "check_connection",
                autospec=True,
            ) as mock_fc_check_connection,
            mock.patch.object(
                sl4f_transport.SL4F,
                "start_server",
                autospec=True,
            ) as mock_sl4f_start_server,
            mock.patch.object(
                sl4f_transport.SL4F,
                "check_connection",
                autospec=True,
            ) as mock_sl4f_check_connection,
        ):
            self.fd_fc_obj = fuchsia_device.FuchsiaDevice(
                device_info=custom_types.DeviceInfo(
                    name=_INPUT_ARGS["device_name"],
                    ip_port=None,
                    serial_socket=_INPUT_ARGS["device_serial_socket"],
                ),
                ffx_config=_INPUT_ARGS["ffx_config"],
                config={
                    "affordances": {
                        "bluetooth": {
                            "implementation": "fuchsia-controller",
                        },
                        "wlan": {
                            "implementation": "fuchsia-controller",
                        },
                    }
                },
            )

            mock_fc_create_context.assert_called_once_with(
                self.fd_fc_obj.fuchsia_controller
            )
            mock_fc_check_connection.assert_called()
            mock_ffx_check_connection.assert_called()
            mock_sl4f_start_server.assert_not_called()
            mock_sl4f_check_connection.assert_not_called()

        with (
            mock.patch.object(
                fuchsia_controller_transport.FuchsiaController,
                "create_context",
                autospec=True,
            ) as mock_fc_create_context,
            mock.patch.object(
                ffx_transport.FFX,
                "check_connection",
                autospec=True,
            ) as mock_ffx_check_connection,
            mock.patch.object(
                fuchsia_controller_transport.FuchsiaController,
                "check_connection",
                autospec=True,
            ) as mock_fc_check_connection,
            mock.patch.object(
                sl4f_transport.SL4F,
                "start_server",
                autospec=True,
            ) as mock_sl4f_start_server,
            mock.patch.object(
                sl4f_transport.SL4F,
                "check_connection",
                autospec=True,
            ) as mock_sl4f_check_connection,
        ):
            self.fd_sl4f_obj = fuchsia_device.FuchsiaDevice(
                device_info=custom_types.DeviceInfo(
                    name=_INPUT_ARGS["device_name"],
                    ip_port=None,
                    serial_socket=_INPUT_ARGS["device_serial_socket"],
                ),
                ffx_config=_INPUT_ARGS["ffx_config"],
                config={
                    "affordances": {
                        "bluetooth": {
                            "implementation": "sl4f",
                        },
                        "wlan": {
                            "implementation": "sl4f",
                        },
                    }
                },
            )

            mock_fc_create_context.assert_called_once_with(
                self.fd_sl4f_obj.fuchsia_controller
            )
            mock_fc_check_connection.assert_called()
            mock_ffx_check_connection.assert_called()
            mock_sl4f_start_server.assert_called_once_with(
                self.fd_sl4f_obj.sl4f
            )
            mock_sl4f_check_connection.assert_called()

    # List all the tests related to __init__
    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_fc_obj, fuchsia_device_interface.FuchsiaDevice
        )
        self.assertIsInstance(
            self.fd_sl4f_obj, fuchsia_device_interface.FuchsiaDevice
        )

    # List all the tests related to transports
    @mock.patch.object(
        fastboot_transport.Fastboot,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_fastboot_transport(self, mock_fastboot_init: mock.Mock) -> None:
        """Test case to make sure fuchsia_device supports fastboot
        transport."""
        self.assertIsInstance(
            self.fd_fc_obj.fastboot,
            fastboot_transport.Fastboot,
        )
        mock_fastboot_init.assert_called_once()

    def test_ffx_transport(self) -> None:
        """Test case to make sure fuchsia_device supports ffx transport."""
        self.assertIsInstance(
            self.fd_fc_obj.ffx,
            ffx_transport.FFX,
        )

    def test_sl4f_transport(self) -> None:
        """Test case to make sure fuchsia_device does not support sl4f
        transport."""
        with (
            mock.patch.object(
                sl4f_transport.SL4F,
                "start_server",
                autospec=True,
            ) as mock_sl4f_start_server,
        ):
            self.assertIsInstance(self.fd_fc_obj.sl4f, sl4f_transport.SL4F)
            mock_sl4f_start_server.assert_called_once_with(self.fd_fc_obj.sl4f)

        self.assertIsInstance(self.fd_sl4f_obj.sl4f, sl4f_transport.SL4F)

    def test_fuchsia_controller_transport(self) -> None:
        """Test case to make sure fuchsia_device supports fuchsia-controller
        transport."""
        self.assertIsInstance(
            self.fd_fc_obj.fuchsia_controller,
            fuchsia_controller_transport.FuchsiaController,
        )

    def test_serial_transport(self) -> None:
        """Test case to make sure fuchsia_device supports serial transport."""
        self.assertIsInstance(
            self.fd_fc_obj.serial,
            serial_using_unix_socket.Serial,
        )

    def test_serial_transport_error(self) -> None:
        """Test case to make sure fuchsia_device raises error when we try to
        access "serial" transport without serial_socket."""

        device_info: custom_types.DeviceInfo = self.fd_fc_obj._device_info

        self.fd_fc_obj._device_info = custom_types.DeviceInfo(
            name=_INPUT_ARGS["device_name"],
            ip_port=None,
            serial_socket=None,
        )

        with self.assertRaisesRegex(
            errors.FuchsiaDeviceError,
            "'serial_socket' arg need to be provided during the init to use Serial affordance",
        ):
            _: serial_interface.Serial = self.fd_fc_obj.serial

        self.fd_fc_obj._device_info = device_info

    # List all the tests related to affordances
    def test_session(self) -> None:
        """Test case to make sure fuchsia_device supports session
        affordance implemented using FFX"""
        self.assertIsInstance(self.fd_fc_obj.session, session_ffx.Session)

    def test_screenshot(self) -> None:
        """Test case to make sure fuchsia_device supports screenshot
        affordance implemented using FFX"""
        self.assertIsInstance(
            self.fd_fc_obj.screenshot,
            screenshot_ffx.Screenshot,
        )

    @mock.patch.object(
        system_power_state_controller_using_starnix.SystemPowerStateControllerUsingStarnix,
        "_run_starnix_console_shell_cmd",
        autospec=True,
    )
    def test_system_power_state_controller(
        self,
        mock_run_starnix_console_shell_cmd: mock.Mock,
    ) -> None:
        """Test case to make sure fuchsia_device supports
        system_power_state_controller affordance implemented using starnix"""
        self.assertIsInstance(
            self.fd_fc_obj.system_power_state_controller,
            system_power_state_controller_using_starnix.SystemPowerStateControllerUsingStarnix,
        )
        mock_run_starnix_console_shell_cmd.assert_called_once()

    @mock.patch.object(
        rtc_fc.Rtc,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_rtc(self, mock_rtc_fc_init: mock.Mock) -> None:
        """Test case to make sure fuchsia_device supports rtc affordance
        implemented using fuchsia-controller"""
        self.assertIsInstance(
            self.fd_fc_obj.rtc,
            rtc_fc.Rtc,
        )
        mock_rtc_fc_init.assert_called_once_with(
            self.fd_fc_obj.rtc,
            fuchsia_controller=self.fd_fc_obj.fuchsia_controller,
            reboot_affordance=self.fd_fc_obj,
        )

    def test_tracing(self) -> None:
        """Test case to make sure fuchsia_device supports tracing affordance
        implemented using fuchsia-controller"""
        self.assertIsInstance(
            self.fd_fc_obj.tracing,
            tracing_fc.Tracing,
        )

    @mock.patch.object(
        ffx_transport.FFX,
        "run",
        autospec=True,
    )
    def test_user_input(
        self,
        mock_ffx_run: mock.Mock,
    ) -> None:
        """Test case to make sure fuchsia_device supports
        user_input affordance."""

        mock_ffx_run.return_value = (
            user_input_fc._INPUT_HELPER_COMPONENT  # pylint: disable=protected-access
        )

        self.assertIsInstance(
            self.fd_fc_obj.user_input,
            user_input_fc.UserInput,
        )

    def test_bluetooth_avrcp_fc_transport(self) -> None:
        """Test case to make sure fuchsia_device only supports
        SL4F based bluetooth_avrcp affordance."""
        with self.assertRaises(NotImplementedError):
            self.fd_fc_obj.bluetooth_avrcp  # pylint: disable=pointless-statement

    @mock.patch.object(
        bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_avrcp_sl4f_transport(
        self, mock_bluetooth_common_init: mock.Mock
    ) -> None:
        """Test case to make sure fuchsia_device only supports
        SL4F based bluetooth_avrcp affordance."""
        self.assertIsInstance(
            self.fd_sl4f_obj.bluetooth_avrcp,
            bluetooth_avrcp_sl4f.BluetoothAvrcp,
        )
        mock_bluetooth_common_init.assert_called_once()

    @mock.patch.object(
        bluetooth_gap_fc.BluetoothGap,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_gap_fc(self, bt_gap_fc_init: mock.Mock) -> None:
        """Test case to make sure fuchsia_device supports
        Fuchsia-Controller based bluetooth_gap affordance."""
        self.assertIsInstance(
            self.fd_fc_obj.bluetooth_gap,
            bluetooth_gap_fc.BluetoothGap,
        )
        bt_gap_fc_init.assert_called_once_with(
            self.fd_fc_obj.bluetooth_gap,
            device_name=self.fd_fc_obj._device_info.name,
            fuchsia_controller=self.fd_fc_obj.fuchsia_controller,
            reboot_affordance=self.fd_fc_obj,
        )

    @mock.patch.object(
        bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_gap_sl4f(
        self, mock_bluetooth_common_init: mock.Mock
    ) -> None:
        """Test case to make sure fuchsia_device supports
        SL4F based bluetooth_gap affordance."""
        self.assertIsInstance(
            self.fd_sl4f_obj.bluetooth_gap,
            bluetooth_gap_sl4f.BluetoothGap,
        )
        mock_bluetooth_common_init.assert_called_once()

    @mock.patch.object(
        ffx_transport.FFX,
        "run",
        return_value="".join(wlan_policy_using_fc._REQUIRED_CAPABILITIES),
        autospec=True,
    )
    @mock.patch.object(
        wlan_policy_using_fc.WlanPolicy,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_wlan_policy_using_fc(
        self,
        wlan_policy_using_fc_init: mock.Mock,
        # pylint: disable-next=unused-argument
        mock_ffx_run: mock.Mock,
    ) -> None:
        """Test case to make sure fuchsia_device supports Fuchsia-Controller based wlan_policy
        affordance."""
        self.assertIsInstance(
            self.fd_fc_obj.wlan_policy,
            wlan_policy_using_fc.WlanPolicy,
        )
        wlan_policy_using_fc_init.assert_called_once_with(
            self.fd_fc_obj.wlan_policy,
            device_name=self.fd_fc_obj._device_info.name,
            ffx=self.fd_fc_obj.ffx,
            fuchsia_controller=self.fd_fc_obj.fuchsia_controller,
            reboot_affordance=self.fd_fc_obj,
            fuchsia_device_close=self.fd_fc_obj,
        )

    @mock.patch.object(
        ffx_transport.FFX,
        "run",
        return_value="".join(wlan_using_fc._REQUIRED_CAPABILITIES),
        autospec=True,
    )
    @mock.patch.object(
        wlan_using_fc.Wlan,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_wlan_using_fc(
        self,
        wlan_using_fc_init: mock.Mock,
        # pylint: disable-next=unused-argument
        mock_ffx_run: mock.Mock,
    ) -> None:
        """Test case to make sure fuchsia_device supports Fuchsia-Controller based wlan
        affordance."""
        self.assertIsInstance(
            self.fd_fc_obj.wlan,
            wlan_using_fc.Wlan,
        )
        wlan_using_fc_init.assert_called_once_with(
            self.fd_fc_obj.wlan,
            device_name=self.fd_fc_obj._device_info.name,
            ffx=self.fd_fc_obj.ffx,
            fuchsia_controller=self.fd_fc_obj.fuchsia_controller,
            reboot_affordance=self.fd_fc_obj,
            fuchsia_device_close=self.fd_fc_obj,
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
        self.assertEqual(self.fd_fc_obj.board, _MOCK_ARGS["board"])
        mock_ffx_get_target_board.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
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
        self.assertEqual(self.fd_fc_obj.manufacturer, "default-manufacturer")

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
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
        self.assertEqual(self.fd_fc_obj.model, "default-model")

    @mock.patch.object(
        ffx_transport.FFX,
        "get_target_product",
        return_value=_MOCK_ARGS["product"],
        autospec=True,
    )
    def test_product(self, mock_ffx_get_target_product: mock.Mock) -> None:
        """Testcase for BaseFuchsiaDevice.product property"""
        self.assertEqual(self.fd_fc_obj.product, _MOCK_ARGS["product"])
        mock_ffx_get_target_product.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
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
        self.assertEqual(self.fd_fc_obj.product_name, "default-product-name")

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_device_info_from_fidl",
        return_value={
            "serial_number": "default-serial-number",
        },
        new_callable=mock.PropertyMock,
    )
    def test_serial_number(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.serial_number property"""
        self.assertEqual(self.fd_fc_obj.serial_number, "default-serial-number")

    # List all the tests related to dynamic properties
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_build_info",
        return_value={
            "version": "1.2.3",
        },
        new_callable=mock.PropertyMock,
    )
    def test_firmware_version(self, *unused_args: Any) -> None:
        """Testcase for BaseFuchsiaDevice.firmware_version property"""
        self.assertEqual(self.fd_fc_obj.firmware_version, "1.2.3")

    # List all the tests related to affordances
    def test_fuchsia_device_is_reboot_capable(self) -> None:
        """Test case to make sure fuchsia device is reboot capable"""
        self.assertIsInstance(
            self.fd_fc_obj, affordances_capable.RebootCapableDevice
        )

    # List all the tests related to public methods
    @parameterized.expand(
        [
            (
                {
                    "label": "no_register_for_on_device_close",
                    "register_for_on_device_close": None,
                    "expected_exception": False,
                },
            ),
            (
                {
                    "label": "register_for_on_device_close_fn_returning_success",
                    "register_for_on_device_close": lambda: None,
                    "expected_exception": False,
                },
            ),
            (
                {
                    "label": "register_for_on_device_close_fn_returning_exception",
                    "register_for_on_device_close": lambda: 1 / 0,
                    "expected_exception": True,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_close(
        self,
        parameterized_dict: dict[str, Any],
    ) -> None:
        """Testcase for FuchsiaDevice.close()"""
        # Reset the `_on_device_close_fns` variable at the beginning of the test
        self.fd_fc_obj._on_device_close_fns = []

        if parameterized_dict["register_for_on_device_close"]:
            self.fd_fc_obj.register_for_on_device_close(
                parameterized_dict["register_for_on_device_close"]
            )
        if parameterized_dict["expected_exception"]:
            with self.assertRaises(Exception):
                self.fd_fc_obj.close()
        else:
            self.fd_fc_obj.close()

        # Reset the `_on_device_close_fns` variable at the end of the test
        self.fd_fc_obj._on_device_close_fns = []

    @mock.patch.object(
        sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(ffx_transport.FFX, "check_connection", autospec=True)
    def test_health_check_fc(
        self,
        mock_ffx_check_connection: mock.Mock,
        mock_fc_check_connection: mock.Mock,
        mock_sl4f_check_connection: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.health_check() when transport is set to
        Fuchsia-Controller"""
        self.fd_fc_obj.health_check()
        mock_ffx_check_connection.assert_called_once_with(self.fd_fc_obj.ffx)
        mock_fc_check_connection.assert_called_once_with(
            self.fd_fc_obj.fuchsia_controller
        )
        mock_sl4f_check_connection.assert_not_called()

    @mock.patch.object(
        sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        ffx_transport.FFX,
        "check_connection",
        autospec=True,
    )
    def test_health_check_sl4f(
        self,
        mock_ffx_check_connection: mock.Mock,
        mock_fc_check_connection: mock.Mock,
        mock_sl4f_check_connection: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.health_check() when transport is set to
        Fuchsia-Controller-Preferred"""
        self.fd_sl4f_obj.health_check()

        mock_ffx_check_connection.assert_called_once_with(self.fd_sl4f_obj.ffx)
        mock_fc_check_connection.assert_called_once_with(
            self.fd_sl4f_obj.fuchsia_controller
        )
        mock_sl4f_check_connection.assert_called_once_with(
            self.fd_sl4f_obj.sl4f
        )

    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        ffx_transport.FFX,
        "check_connection",
        side_effect=errors.FfxConnectionError("ffx connection error"),
        autospec=True,
    )
    def test_health_check_exception(
        self,
        mock_ffx_check_connection: mock.Mock,
        mock_fc_check_connection: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.health_check() raising HealthCheckError"""
        with self.assertRaises(errors.HealthCheckError):
            self.fd_fc_obj.health_check()

        mock_ffx_check_connection.assert_called_once_with(self.fd_fc_obj.ffx)
        mock_fc_check_connection.assert_not_called()

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
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_send_log_command",
        autospec=True,
    )
    def test_log_message_to_device(
        self,
        parameterized_dict: dict[str, Any],
        mock_send_log_command: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.log_message_to_device()"""
        self.fd_fc_obj.log_message_to_device(
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"],
        )

        mock_send_log_command.assert_called_with(
            self.fd_fc_obj,
            tag="lacewing",
            message=mock.ANY,
            level=parameterized_dict["log_level"],
        )

    @parameterized.expand(
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
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    @mock.patch.object(
        sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    def test_on_device_boot_fc(
        self,
        parameterized_dict: dict[str, Any],
        mock_sl4f_start_server: mock.Mock,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
    ) -> None:
        """Testcase for BaseFuchsiaDevice.on_device_boot() when transport is set to
        Fuchsia-Controller"""
        # Reset the `_on_device_boot_fns` variable at the beginning of the test
        self.fd_fc_obj._on_device_boot_fns = []

        if parameterized_dict["register_for_on_device_boot"]:
            self.fd_fc_obj.register_for_on_device_boot(
                parameterized_dict["register_for_on_device_boot"]
            )
        if parameterized_dict["expected_exception"]:
            with self.assertRaises(Exception):
                self.fd_fc_obj.on_device_boot()
        else:
            self.fd_fc_obj.on_device_boot()

        # Reset the `_on_device_boot_fns` variable at the end of the test
        self.fd_fc_obj._on_device_boot_fns = []

        mock_fc_create_context.assert_called_once()
        mock_health_check.assert_called_once()
        mock_sl4f_start_server.assert_not_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    @mock.patch.object(
        sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    def test_on_device_boot(
        self,
        mock_sl4f_start_server: mock.Mock,
        mock_fc_create_context: mock.Mock,
        mock_sl4f_health_check: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.on_device_boot() when transport is set to
        Fuchsia-Controller-Preferred"""
        self.fd_sl4f_obj.on_device_boot()

        mock_sl4f_start_server.assert_called_once_with(self.fd_sl4f_obj.sl4f)
        mock_fc_create_context.assert_called_once_with(
            self.fd_sl4f_obj.fuchsia_controller
        )
        mock_sl4f_health_check.assert_called_once_with(self.fd_sl4f_obj)

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "on_device_boot", autospec=True
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "wait_for_online",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "wait_for_offline",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
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
        self.fd_fc_obj.power_cycle(power_switch=power_switch, outlet=5)

        self.assertEqual(mock_log_message_to_device.call_count, 2)
        mock_wait_for_offline.assert_called()
        mock_wait_for_online.assert_called()
        mock_on_device_boot.assert_called()

    @mock.patch.object(
        fuchsia_device.FuchsiaDevice, "on_device_boot", autospec=True
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "wait_for_online",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "wait_for_offline",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "_send_reboot_command",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
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
        self.fd_fc_obj.reboot()

        self.assertEqual(mock_log_message_to_device.call_count, 2)
        mock_send_reboot_command.assert_called()
        mock_wait_for_offline.assert_called()
        mock_wait_for_online.assert_called()
        mock_on_device_boot.assert_called()

    def test_register_for_on_device_boot(self) -> None:
        """Testcase for BaseFuchsiaDevice.register_for_on_device_boot()"""
        self.fd_fc_obj.register_for_on_device_boot(fn=lambda: None)

    def test_register_for_on_device_close(self) -> None:
        """Testcase for BaseFuchsiaDevice.register_for_on_device_close()"""
        self.fd_fc_obj.register_for_on_device_boot(fn=lambda: None)
        self.fd_fc_obj.close()

    @parameterized.expand(
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
        fuchsia_device.FuchsiaDevice,
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
            snapshot_file_path: str = self.fd_fc_obj.snapshot(
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
                f"{directory}/Snapshot_{self.fd_fc_obj.device_name}_.*.zip",
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
        self.fd_fc_obj.wait_for_offline()

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
            self.fd_fc_obj.wait_for_offline()

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
        self.fd_fc_obj.wait_for_online()

        mock_ffx_wait_for_rcs_connection.assert_called()

    @mock.patch.object(
        ffx_transport.FFX,
        "wait_for_rcs_connection",
        side_effect=errors.FfxCommandError("error"),
        autospec=True,
    )
    def test_wait_for_online_fail(
        self, mock_ffx_wait_for_rcs_connection: mock.Mock
    ) -> None:
        """Testcase for BaseFuchsiaDevice.wait_for_online() failure case"""
        with self.assertRaisesRegex(
            errors.FuchsiaDeviceError, "failed to go online"
        ):
            self.fd_fc_obj.wait_for_online()

        mock_ffx_wait_for_rcs_connection.assert_called()

    # List all the tests related to private properties
    @mock.patch.object(
        f_buildinfo.Provider.Client,
        "get_build_info",
        new_callable=mock.AsyncMock,
        return_value=f_buildinfo.ProviderGetBuildInfoResponse(
            build_info=_MOCK_DEVICE_PROPERTIES["build_info"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_build_info(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_buildinfo_provider: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._build_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_fc_obj._build_info, _MOCK_DEVICE_PROPERTIES["build_info"]
        )

        mock_fc_connect_device_proxy.assert_called_once()
        mock_buildinfo_provider.assert_called()

    @mock.patch.object(
        f_buildinfo.Provider.Client,
        "get_build_info",
        new_callable=mock.AsyncMock,
        return_value=f_buildinfo.ProviderGetBuildInfoResponse(
            build_info=_MOCK_DEVICE_PROPERTIES["build_info"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_build_info_error(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_buildinfo_provider: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._build_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_buildinfo_provider.side_effect = ZxStatus(
            ZxStatus.ZX_ERR_INVALID_ARGS
        )
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _ = self.fd_fc_obj._build_info

        mock_fc_connect_device_proxy.assert_called_once()

    @mock.patch.object(
        f_hwinfo.Device.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.DeviceGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["device_info_from_fidl"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_device_info_from_fidl(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_hwinfo_device: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._device_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_fc_obj._device_info_from_fidl,
            _MOCK_DEVICE_PROPERTIES["device_info_from_fidl"],
        )

        mock_fc_connect_device_proxy.assert_called_once()
        mock_hwinfo_device.assert_called()

    @mock.patch.object(
        f_hwinfo.Device.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.DeviceGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["device_info_from_fidl"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_device_info_from_fidl_error(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_hwinfo_device: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._device_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_hwinfo_device.side_effect = ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _: dict[str, Any] = self.fd_fc_obj._device_info_from_fidl

        mock_fc_connect_device_proxy.assert_called_once()

    @mock.patch.object(
        f_hwinfo.Product.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.ProductGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["product_info"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_product_info(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_hwinfo_product: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._product_info property"""
        # pylint: disable=protected-access
        self.assertEqual(
            self.fd_fc_obj._product_info,
            _MOCK_DEVICE_PROPERTIES["product_info"],
        )

        mock_fc_connect_device_proxy.assert_called_once()
        mock_hwinfo_product.assert_called()

    @mock.patch.object(
        f_hwinfo.Product.Client,
        "get_info",
        new_callable=mock.AsyncMock,
        return_value=f_hwinfo.ProductGetInfoResponse(
            info=_MOCK_DEVICE_PROPERTIES["product_info"]
        ),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_product_info_error(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_hwinfo_product: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._product_info property when the get_info
        FIDL call raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_hwinfo_product.side_effect = ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            _ = self.fd_fc_obj._product_info

        mock_fc_connect_device_proxy.assert_called_once()

    # List all the tests related to private methods
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
    @mock.patch.object(
        fd_remotecontrol.RemoteControl.Client,
        "log_message",
        new_callable=mock.AsyncMock,
    )
    def test_send_log_command(
        self,
        parameterized_dict: dict[str, Any],
        mock_rcs_log_message: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._send_log_command()"""
        self.fd_fc_obj.fuchsia_controller.ctx = mock.Mock()
        # pylint: disable=protected-access
        self.fd_fc_obj._send_log_command(
            tag="test",
            level=parameterized_dict["log_level"],
            message=parameterized_dict["log_message"],
        )

        mock_rcs_log_message.assert_called()

    @mock.patch.object(
        fd_remotecontrol.RemoteControl.Client,
        "log_message",
        new_callable=mock.AsyncMock,
    )
    def test_send_log_command_error(
        self, mock_rcs_log_message: mock.Mock
    ) -> None:
        """Testcase for FuchsiaDevice._send_log_command() when the log FIDL call
        raises an error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        self.fd_fc_obj.fuchsia_controller.ctx = mock.Mock()

        mock_rcs_log_message.side_effect = ZxStatus(
            ZxStatus.ZX_ERR_INVALID_ARGS
        )
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            self.fd_fc_obj._send_log_command(
                tag="test", level=custom_types.LEVEL.ERROR, message="test"
            )

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_send_reboot_command(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_admin_reboot: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command()"""
        # pylint: disable=protected-access
        self.fd_fc_obj._send_reboot_command()

        mock_fc_connect_device_proxy.assert_called()
        mock_admin_reboot.assert_called()

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_send_reboot_command_error(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_admin_reboot: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command() when the reboot
        FIDL call raises a non-ZX_ERR_PEER_CLOSED error.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        mock_admin_reboot.side_effect = ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS)
        with self.assertRaises(errors.FuchsiaControllerError):
            # pylint: disable=protected-access
            self.fd_fc_obj._send_reboot_command()

        mock_fc_connect_device_proxy.assert_called()
        mock_admin_reboot.assert_called()

    @mock.patch.object(
        fhp_statecontrol.Admin.Client,
        "reboot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    def test_send_reboot_command_error_is_peer_closed(
        self,
        mock_fc_connect_device_proxy: mock.Mock,
        mock_admin_reboot: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice._send_reboot_command() when the reboot
        FIDL call raises a ZX_ERR_PEER_CLOSED error.  This error should not
        result in `FuchsiaControllerError` being raised."""
        mock_admin_reboot.side_effect = ZxStatus(ZxStatus.ZX_ERR_PEER_CLOSED)
        # pylint: disable=protected-access
        self.fd_fc_obj._send_reboot_command()

        mock_fc_connect_device_proxy.assert_called()
        mock_admin_reboot.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_io.File.Client,
        "get_attr",
        new_callable=mock.AsyncMock,
        return_value=_file_attr_resp(ZxStatus(ZxStatus.ZX_OK), 15),
    )
    @mock.patch.object(
        f_io.File.Client,
        "read",
        new_callable=mock.AsyncMock,
        side_effect=[
            # Read 15 bytes over multiple responses.
            _file_read_result([0] * 5),
            _file_read_result([0] * 5),
            _file_read_result([0] * 5),
            # Send empty response to signal read completion.
            _file_read_result([]),
        ],
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command()"""
        # pylint: disable=protected-access
        data = self.fd_fc_obj._send_snapshot_command()
        self.assertEqual(len(data), 15)

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
        # Raise arbitrary failure.
        side_effect=ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command_get_snapshot_error(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command() when the
        get_snapshot FIDL call raises an exception.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        # pylint: disable=protected-access
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fd_fc_obj._send_snapshot_command()

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_io.File.Client,
        "get_attr",
        new_callable=mock.AsyncMock,
        # Raise arbitrary failure.
        side_effect=ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command_get_attr_error(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command() when the get_attr
        FIDL call raises an exception.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        # pylint: disable=protected-access
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fd_fc_obj._send_snapshot_command()

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_io.File.Client,
        "get_attr",
        new_callable=mock.AsyncMock,
        return_value=_file_attr_resp(ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS), 0),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command_get_attr_status_not_ok(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command() when the get_attr
        FIDL call returns a non-OK status code.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        # pylint: disable=protected-access
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fd_fc_obj._send_snapshot_command()

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_io.File.Client,
        "get_attr",
        new_callable=mock.AsyncMock,
        return_value=_file_attr_resp(ZxStatus(ZxStatus.ZX_OK), 15),
    )
    @mock.patch.object(
        f_io.File.Client,
        "read",
        new_callable=mock.AsyncMock,
        side_effect=ZxStatus(ZxStatus.ZX_ERR_INVALID_ARGS),
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command_read_error(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command() when the read
        FIDL call raises an exception.
        ZX_ERR_INVALID_ARGS was chosen arbitrarily for this purpose."""
        # pylint: disable=protected-access
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fd_fc_obj._send_snapshot_command()

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()

    @mock.patch.object(
        f_feedback.DataProvider.Client,
        "get_snapshot",
        new_callable=mock.AsyncMock,
    )
    @mock.patch.object(
        f_io.File.Client,
        "get_attr",
        new_callable=mock.AsyncMock,
        # File reports size of 15 bytes.
        return_value=_file_attr_resp(ZxStatus(ZxStatus.ZX_OK), 15),
    )
    @mock.patch.object(
        f_io.File.Client,
        "read",
        new_callable=mock.AsyncMock,
        # Only 5 bytes are read.
        side_effect=[
            _file_read_result([0] * 5),
            _file_read_result([]),
        ],
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "connect_device_proxy",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_device.FuchsiaDevice,
        "health_check",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "create_context",
        autospec=True,
    )
    def test_send_snapshot_command_size_mismatch(
        self,
        mock_fc_create_context: mock.Mock,
        mock_health_check: mock.Mock,
        mock_fc_connect_device_proxy: mock.Mock,
        *unused_args: Any,
    ) -> None:
        """Testcase for FuchsiaDevice._send_snapshot_command() when the number
        of bytes read from channel doesn't match the file's content size."""
        # pylint: disable=protected-access
        with self.assertRaises(errors.FuchsiaControllerError):
            self.fd_fc_obj._send_snapshot_command()

        mock_fc_create_context.assert_called()
        mock_health_check.assert_called()
        mock_fc_connect_device_proxy.assert_called()


if __name__ == "__main__":
    unittest.main()
