#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for
honeydew.fuchsia_device.fuchsia_controller.fuchsia_device.py."""

import unittest
from typing import Any
from unittest import mock

import fuchsia_controller_py as fuchsia_controller

from honeydew.affordances.sl4f.bluetooth import bluetooth_common
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_avrcp as bluetooth_avrcp_sl4f,
)
from honeydew.affordances.sl4f.bluetooth.profiles import (
    bluetooth_gap as bluetooth_gap_sl4f,
)
from honeydew.affordances.sl4f.wlan import wlan as wlan_sl4f
from honeydew.affordances.sl4f.wlan import wlan_policy as wlan_policy_sl4f
from honeydew.fuchsia_device.fuchsia_controller_preferred import (
    fuchsia_device as fc_preferred_fuchsia_device,
)
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import ffx as ffx_transport
from honeydew.transports import (
    fuchsia_controller as fuchsia_controller_transport,
)
from honeydew.transports import sl4f as sl4f_transport
from honeydew.transports import ssh as ssh_transport
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


class FuchsiaDeviceFCPreferredTests(unittest.TestCase):
    """Unit tests for
    honeydew.fuchsia_device.fuchsia_controller_preferred.fuchsia_device.py."""

    def setUp(self) -> None:
        with (
            mock.patch.object(
                fuchsia_controller_transport.FuchsiaController,
                "create_context",
                autospec=True,
            ) as mock_fc_create_context,
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
            self.fd_obj = fc_preferred_fuchsia_device.FuchsiaDevice(
                device_name=_INPUT_ARGS["device_name"],
                ssh_private_key=_INPUT_ARGS["ssh_private_key"],
                ffx_config=_INPUT_ARGS["ffx_config"],
            )

            mock_fc_create_context.assert_called_once_with(
                self.fd_obj.fuchsia_controller
            )
            mock_ffx_check_connection.assert_called_once_with(self.fd_obj.ffx)
            mock_ssh_check_connection.assert_called_once_with(self.fd_obj.ssh)
            mock_fc_check_connection.assert_called_once_with(
                self.fd_obj.fuchsia_controller
            )
            mock_sl4f_start_server.assert_called_once_with(self.fd_obj.sl4f)
            mock_sl4f_check_connection.assert_called_once_with(self.fd_obj.sl4f)

    def test_device_is_a_fuchsia_device(self) -> None:
        """Test case to make sure DUT is a fuchsia device"""
        self.assertIsInstance(
            self.fd_obj, fuchsia_device_interface.FuchsiaDevice
        )

    # List all the tests related to transports
    def test_sl4f_transport(self) -> None:
        """Test case to make sure fc_preferred_fuchsia_device supports SL4F
        transport."""
        self.assertIsInstance(self.fd_obj.sl4f, sl4f_transport.SL4F)

    # List all the tests related to affordances
    @mock.patch.object(
        bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_avrcp(
        self, mock_bluetooth_common_init: mock.Mock
    ) -> None:
        """Test case to make sure fc_preferred_fuchsia_device supports
        bluetooth_avrcp affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.bluetooth_avrcp,
            bluetooth_avrcp_sl4f.BluetoothAvrcp,
        )
        mock_bluetooth_common_init.assert_called_once()

    @mock.patch.object(
        bluetooth_common.BluetoothCommon,
        "__init__",
        autospec=True,
        return_value=None,
    )
    def test_bluetooth_gap(self, mock_bluetooth_common_init: mock.Mock) -> None:
        """Test case to make sure fc_preferred_fuchsia_device supports
        bluetooth_gap affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.bluetooth_gap,
            bluetooth_gap_sl4f.BluetoothGap,
        )
        mock_bluetooth_common_init.assert_called_once()

    def test_wlan_policy(self) -> None:
        """Test case to make sure fc_preferred_fuchsia_device supports
        wlan_policy affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.wlan_policy,
            wlan_policy_sl4f.WlanPolicy,
        )

    def test_wlan(self) -> None:
        """Test case to make sure fc_preferred_fuchsia_device supports
        wlan affordance implemented using SL4F transport."""
        self.assertIsInstance(
            self.fd_obj.wlan,
            wlan_sl4f.Wlan,
        )

    # List all the tests related to public methods
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
    @mock.patch.object(
        ssh_transport.SSH,
        "check_connection",
        autospec=True,
    )
    def test_health_check(
        self,
        mock_ssh_check_connection: mock.Mock,
        mock_ffx_check_connection: mock.Mock,
        mock_fc_check_connection: mock.Mock,
        mock_sl4f_check_connection: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.health_check()"""
        self.fd_obj.health_check()

        mock_ffx_check_connection.assert_called_once_with(self.fd_obj.ffx)
        mock_ssh_check_connection.assert_called_once_with(self.fd_obj.ssh)
        mock_fc_check_connection.assert_called_once_with(
            self.fd_obj.fuchsia_controller
        )
        mock_sl4f_check_connection.assert_called_once_with(self.fd_obj.sl4f)

    @mock.patch.object(
        fc_preferred_fuchsia_device.FuchsiaDevice, "health_check", autospec=True
    )
    @mock.patch.object(
        sl4f_transport.SL4F,
        "start_server",
        autospec=True,
    )
    def test_on_device_boot(
        self,
        mock_sl4f_start_server: mock.Mock,
        mock_fc_preferred_health_check: mock.Mock,
    ) -> None:
        """Testcase for FuchsiaDevice.on_device_boot()"""
        self.fd_obj.on_device_boot()

        mock_sl4f_start_server.assert_called_once_with(self.fd_obj.sl4f)
        mock_fc_preferred_health_check.assert_called_once_with(self.fd_obj)


if __name__ == "__main__":
    unittest.main()
