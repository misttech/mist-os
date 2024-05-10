#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.__init__.py."""

import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller

import honeydew
from honeydew import errors
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)
from honeydew.transports import sl4f as sl4f_transport
from honeydew.transports import ssh as ssh_transport
from honeydew.typing import custom_types

_INPUT_ARGS: dict[str, Any] = {
    "ffx_config": custom_types.FFXConfig(
        isolate_dir=fuchsia_controller.IsolateDir("/tmp/isolate"),
        logs_dir="/tmp/logs",
        binary_path="/bin/ffx",
        logs_level="debug",
        mdns_enabled=False,
        subtools_search_path=None,
    ),
}


# pylint: disable=protected-access
class InitTests(unittest.TestCase):
    """Unit tests for honeydew.__init__.py."""

    # List all the tests related to public methods
    @mock.patch.object(
        sl4f_transport.SL4F,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(ssh_transport.SSH, "check_connection", autospec=True)
    @mock.patch.object(ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "check_connection",
        autospec=True,
    )
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    def test_create_device_return_fc_device(
        self,
        mock_fc_context: mock.Mock,
        mock_ssh_check_connection: mock.Mock,
        mock_ffx_check_connection: mock.Mock,
        mock_fc_check_connection: mock.Mock,
        mock_sl4f_check_connection: mock.Mock,
    ) -> None:
        """Test case for honeydew.create_device() where it returns
        Fuchsia-Controller based fuchsia device object."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-emulator",
                ssh_private_key="/tmp/pkey",
                transport=custom_types.TRANSPORT.FUCHSIA_CONTROLLER,
                ffx_config=_INPUT_ARGS["ffx_config"],
            ),
            fc_fuchsia_device.FuchsiaDevice,
        )

        mock_fc_context.assert_called_once_with(
            config={
                "log.dir": _INPUT_ARGS["ffx_config"].logs_dir,
                "log.level": _INPUT_ARGS["ffx_config"].logs_level,
                "daemon.autostart": "false",
            },
            isolate_dir=_INPUT_ARGS["ffx_config"].isolate_dir,
            target="fuchsia-emulator",
        )
        mock_fc_check_connection.assert_called()

        mock_ffx_check_connection.assert_called()

        mock_ssh_check_connection.assert_called()

        mock_sl4f_check_connection.assert_not_called()

    @mock.patch.object(ssh_transport.SSH, "check_connection", autospec=True)
    @mock.patch.object(ffx_transport.FFX, "check_connection", autospec=True)
    @mock.patch.object(
        ffx_transport.FFX,
        "add_target",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "check_connection",
        autospec=True,
    )
    @mock.patch.object(
        fuchsia_controller_transport.FuchsiaController,
        "add_target",
        autospec=True,
    )
    @mock.patch("fuchsia_controller_py.Context", autospec=True)
    def test_create_device_using_device_ip_port(
        self,
        mock_fc_context: mock.Mock,
        mock_fc_add_target: mock.Mock,
        mock_fc_check_connection: mock.Mock,
        mock_ffx_add_target: mock.Mock,
        mock_ffx_check_connection: mock.Mock,
        mock_ssh_check_connection: mock.Mock,
    ) -> None:
        """Test case for honeydew.create_device() where it returns a device
        from an IpPort."""
        self.assertIsInstance(
            honeydew.create_device(
                device_name="fuchsia-1234",
                ssh_private_key="/tmp/pkey",
                transport=custom_types.TRANSPORT.FUCHSIA_CONTROLLER,
                ffx_config=_INPUT_ARGS["ffx_config"],
                device_ip_port=custom_types.IpPort.create_using_ip_and_port(
                    "[::1]:8088"
                ),
            ),
            fc_fuchsia_device.FuchsiaDevice,
        )

        mock_fc_context.assert_called_once_with(
            config={
                "log.dir": _INPUT_ARGS["ffx_config"].logs_dir,
                "log.level": _INPUT_ARGS["ffx_config"].logs_level,
                "daemon.autostart": "false",
            },
            isolate_dir=_INPUT_ARGS["ffx_config"].isolate_dir,
            target="::1",
        )
        mock_fc_add_target.assert_called()
        mock_fc_check_connection.assert_called()

        mock_ffx_add_target.assert_called()
        mock_ffx_check_connection.assert_called()

        mock_ssh_check_connection.assert_called()

    @mock.patch.object(
        fc_fuchsia_device.FuchsiaDevice,
        "__init__",
        side_effect=errors.FuchsiaControllerConnectionError("Error"),
        autospec=True,
    )
    def test_create_device_using_device_ip_port_throws_error(
        self,
        mock_fc_fuchsia_device: mock.Mock,
    ) -> None:
        """Test case for honeydew.create_device() where it raises an error."""
        with self.assertRaises(errors.FuchsiaDeviceError):
            honeydew.create_device(
                device_name="fuchsia-1234",
                transport=custom_types.TRANSPORT.FUCHSIA_CONTROLLER,
                ssh_private_key="/tmp/pkey",
                device_ip_port=custom_types.IpPort.create_using_ip_and_port(
                    "[::1]:8088"
                ),
                ffx_config=_INPUT_ARGS["ffx_config"],
            )

        mock_fc_fuchsia_device.assert_called()


if __name__ == "__main__":
    unittest.main()
