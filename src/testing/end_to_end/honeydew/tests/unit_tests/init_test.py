#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.__init__.py."""

import subprocess
import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fcp
from parameterized import parameterized

import honeydew
from honeydew import custom_types, errors
from honeydew.fuchsia_device import base_fuchsia_device
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.fuchsia_device.sl4f import fuchsia_device as sl4f_fuchsia_device

_MOCK_ARGS: dict[str, Any] = {
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fcp.IsolateDir(), logs_dir="/tmp/logs"
    ),
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


# pylint: disable=protected-access
class InitTests(unittest.TestCase):
    """Unit tests for honeydew.__init__.py."""

    # List all the tests related to public methods in alphabetical order
    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    def test_create_device_return_sl4f_device(
        self,
        mock_sl4f_start_server,
        mock_sl4f_check_connection,
        mock_ssh_check_connection,
        mock_ffx_check_connection,
    ) -> None:
        """Test case for honeydew.create_device() where it returns SL4F based
        fuchsia device object."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-emulator",
                transport=honeydew.transports.TRANSPORT.SL4F,
                ssh_private_key="/tmp/pkey",
            ),
            sl4f_fuchsia_device.FuchsiaDevice,
        )

        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()
        mock_ssh_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    @mock.patch.object(
        base_fuchsia_device.ffx_transport.FFX, "check_connection", autospec=True
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    @mock.patch(
        "honeydew.transports.ffx.get_config",
        return_value=_MOCK_ARGS["ffx_config"],
        autospec=True,
    )
    def test_create_device_return_fc_device(
        self,
        mock_ffx_get_config,
        mock_fc_context,
        mock_ssh_check_connection,
        mock_ffx_check_connection,
    ) -> None:
        """Test case for honeydew.create_device() where it returns
        Fuchsia-Controller based fuchsia device object."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-emulator",
                ssh_private_key="/tmp/pkey",
                transport=honeydew.transports.TRANSPORT.FUCHSIA_CONTROLLER,
            ),
            fc_fuchsia_device.FuchsiaDevice,
        )

        mock_ffx_get_config.assert_called_once()
        mock_fc_context.assert_called_once_with(
            config=mock.ANY,
            isolate_dir=_MOCK_ARGS["ffx_config"].isolate_dir,
            target="fuchsia-emulator",
        )
        mock_ssh_check_connection.assert_called()
        mock_ffx_check_connection.assert_called()

    @mock.patch("honeydew.ffx_transport.FFX", autospec=True)
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    def test_create_device_using_device_ip_port(
        self,
        mock_ssh_check_connection,
        mock_sl4f_start_server,
        mock_sl4f_check_connection,
        mock_ffx,
    ) -> None:
        """Test case for honeydew.create_device() where it returns a device
        from an IpPort."""
        device_name = "fuchsia-1234"
        device_ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "[::1]:8088"
        )

        mock_ffx.return_value = mock_ffx
        mock_ffx.get_target_name.return_value = device_name
        mock_ffx.get_target_information.side_effect = subprocess.TimeoutExpired(
            cmd="foo", timeout=10
        )

        self.assertIsInstance(
            honeydew.create_device(
                device_name=device_name,
                transport=honeydew.transports.TRANSPORT.SL4F,
                ssh_private_key="/tmp/pkey",
                device_ip_port=device_ip_port,
            ),
            sl4f_fuchsia_device.FuchsiaDevice,
        )

        mock_ffx.add_target.assert_called_once()
        mock_ffx.get_target_name.assert_called()
        mock_ffx.check_connection.assert_called()

        mock_ssh_check_connection.assert_called()
        mock_sl4f_start_server.assert_called()
        mock_sl4f_check_connection.assert_called()

    @mock.patch("honeydew.ffx_transport.FFX", autospec=True)
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    def test_create_device_using_device_ip_port_throws_on_add_error(
        self,
        mock_ssh_check_connection,
        mock_sl4f_start_server,
        mock_sl4f_check_connection,
        mock_ffx,
    ) -> None:
        """Test case for honeydew.create_device() where it raises an error due
        to an exception in add_target."""
        device_name = "fuchsia-1234"
        device_ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "[::1]:8022"
        )

        mock_ffx.return_value = mock_ffx
        mock_ffx.add_target.side_effect = subprocess.CalledProcessError(
            returncode=1, cmd="ffx target add [::1]:8022 "
        )

        mock_ffx.get_target_information.side_effect = subprocess.TimeoutExpired(
            cmd="foo", timeout=10
        )

        with self.assertRaises(errors.FfxCommandError):
            honeydew.create_device(
                device_name=device_name,
                transport=honeydew.transports.TRANSPORT.SL4F,
                ssh_private_key="/tmp/pkey",
                device_ip_port=device_ip_port,
            )

        mock_ffx.get_target_information.assert_called_once()
        mock_ffx.add_target.assert_called_once()
        mock_ffx.get_target_name.assert_not_called()

        mock_ssh_check_connection.assert_not_called()
        mock_sl4f_start_server.assert_not_called()
        mock_sl4f_check_connection.assert_not_called()

    @mock.patch("honeydew.ffx_transport.FFX", autospec=True)
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    def test_create_device_using_device_ip_port_throws_on_different_target_names(
        self,
        mock_ssh_check_connection,
        mock_sl4f_start_server,
        mock_sl4f_check_connection,
        mock_ffx,
    ) -> None:
        """Test case for honeydew.create_device() where it raises an exception
        because the returned target name is different from the given one."""

        device_name = "fuchsia-1234"
        device_ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "[::1]:8022"
        )

        mock_ffx.return_value = mock_ffx
        mock_ffx.get_target_name.return_value = "not-a-fuchsia-name"
        mock_ffx.get_target_information.side_effect = subprocess.TimeoutExpired(
            cmd="foo", timeout=10
        )

        with self.assertRaises(errors.FfxCommandError):
            honeydew.create_device(
                device_name=device_name,
                transport=honeydew.transports.TRANSPORT.SL4F,
                ssh_private_key="/tmp/pkey",
                device_ip_port=device_ip_port,
            )

        mock_ffx.get_target_information.assert_called_once()
        mock_ffx.add_target.assert_called_once()
        mock_ffx.get_target_name.assert_called_once()

        mock_ssh_check_connection.assert_not_called()
        mock_sl4f_start_server.assert_not_called()
        mock_sl4f_check_connection.assert_not_called()

    @mock.patch("honeydew.ffx_transport.FFX", autospec=True)
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        honeydew.sl4f_fuchsia_device.sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    @mock.patch.object(
        base_fuchsia_device.ssh_transport.SSH, "check_connection", autospec=True
    )
    def test_create_device_using_device_ip_port_skips_add_on_existing_target(
        self,
        mock_ssh_check_connection,
        mock_sl4f_start_server,
        mock_sl4f_check_connection,
        mock_ffx,
    ) -> None:
        """Test case for honeydew.create_device() where it skips adding the
        target since it is already registered."""

        device_name = "fuchsia-1234"
        device_ip_port: custom_types.IpPort = custom_types.IpPort.parse(
            "[::1]:8022"
        )

        mock_ffx.return_value = mock_ffx
        mock_ffx.get_target_name.return_value = "fuchsia-1234"
        mock_ffx.get_target_information.return_value = {}

        honeydew.create_device(
            device_name=device_name,
            transport=honeydew.transports.TRANSPORT.SL4F,
            ssh_private_key="/tmp/pkey",
            device_ip_port=device_ip_port,
        )

        mock_ffx.get_target_information.assert_called_once()
        mock_ffx.add_target.assert_not_called()
        mock_ffx.get_target_name.assert_called_once()

        mock_ssh_check_connection.assert_called_once()
        mock_sl4f_start_server.assert_called_once()
        mock_sl4f_check_connection.assert_called_once()

    @mock.patch(
        "honeydew._get_device_class",
        side_effect=RuntimeError("mock runtime error"),
        autospec=True,
    )
    def test_create_device_fuchsia_device_error_exception(
        self, mock_get_device_class
    ) -> None:
        """Test case for honeydew.create_device() raising FuchsiaDeviceError
        exception."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            honeydew.create_device(
                device_name="fuchsia-1234",
                transport=honeydew.transports.TRANSPORT.SL4F,
            )

        mock_get_device_class.assert_called()


if __name__ == "__main__":
    unittest.main()
