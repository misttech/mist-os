# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.sl4f.bluetooth_avrcp.py."""

import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

from parameterized import param, parameterized

from honeydew.affordances.connectivity.bluetooth.avrcp import avrcp_using_sl4f
from honeydew.affordances.connectivity.bluetooth.utils import (
    types as bluetooth_types,
)
from honeydew.interfaces.device_classes import affordances_capable
from honeydew.transports import sl4f as sl4f_transport

_SAMPLE_RECEIVED_REQUESTS: dict[str, Any] = {
    "id": "",
    "result": ["play"],
    "error": None,
}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


BluetoothAvrcpCommand = bluetooth_types.BluetoothAvrcpCommand


# pylint: disable=protected-access
class BluetoothAvrcpSL4FTests(unittest.TestCase):
    """Unit tests for
    honeydew.affordances.sl4f.bluetooth.bluetooth_avrcp.py.
    """

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.reboot_affordance_obj = mock.MagicMock(
            spec=affordances_capable.RebootCapableDevice
        )

        self.bluetooth_obj = avrcp_using_sl4f.AvrcpUsingSl4f(
            device_name="fuchsia-emulator",
            sl4f=self.sl4f_obj,
            reboot_affordance=self.reboot_affordance_obj,
        )

        self.sl4f_obj.run.assert_called()
        self.sl4f_obj.reset_mock()

    def test_avrcp_init(self) -> None:
        """Test for Bluetooth.avrcp_init() method."""
        self.bluetooth_obj.init_avrcp(target_id="0")

        self.sl4f_obj.run.assert_called()

    def test_list_received_requests(self) -> None:
        """Test for Bluetooth.list_received_requests() method."""
        self.sl4f_obj.run.return_value = _SAMPLE_RECEIVED_REQUESTS
        res = self.bluetooth_obj.list_received_requests()
        self.sl4f_obj.run.assert_called()
        assert "play" in res

    def test_publish_mock_player(self) -> None:
        """Test for Bluetooth.publish_mock_player() method."""
        self.bluetooth_obj.publish_mock_player()

        self.sl4f_obj.run.assert_called()

    @parameterized.expand(
        [
            ({"label": "play_command", "command": BluetoothAvrcpCommand.PLAY},),
            (
                {
                    "label": "pause_command",
                    "command": BluetoothAvrcpCommand.PAUSE,
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    def test_send_avrcp_command(
        self, parameterized_dict: dict[str, Any]
    ) -> None:
        """Test for Bluetooth.send_avrcp_command() method."""
        self.bluetooth_obj.send_avrcp_command(
            command=parameterized_dict["command"]
        )

        self.sl4f_obj.run.assert_called()

    def test_stop_mock_player(self) -> None:
        """Test for Bluetooth.stop_mock_player() method"""
        self.bluetooth_obj.stop_mock_player()

        self.sl4f_obj.run.assert_called()
