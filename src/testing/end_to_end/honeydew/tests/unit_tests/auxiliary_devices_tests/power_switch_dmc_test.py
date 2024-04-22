#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.auxiliary_devices.power_switch_dmc.py."""

import os
import subprocess
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

from parameterized import param, parameterized

from honeydew.auxiliary_devices import power_switch_dmc
from honeydew.interfaces.auxiliary_devices import power_switch

_MOCK_OS_ENVIRON: dict[str, str] = {"DMC_PATH": "/tmp/foo/bar"}


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class PowerSwitchDmcTests(unittest.TestCase):
    """Unit tests for honeydew.auxiliary_devices.power_switch_dmc.py."""

    def setUp(self) -> None:
        super().setUp()

        with mock.patch.dict(os.environ, _MOCK_OS_ENVIRON, clear=True):
            self.power_switch_dmc_obj: power_switch_dmc.PowerSwitchDmc = (
                power_switch_dmc.PowerSwitchDmc(device_name="fx-emu")
            )

    def test_instantiate_power_switch_dmc_when_dmc_path_not_set(self) -> None:
        """Test case to make sure creating PowerSwitchDmc when DMC_PATH not set
        will result in a failure."""
        with self.assertRaisesRegex(
            power_switch_dmc.PowerSwitchDmcError,
            "environmental variable is not set",
        ):
            power_switch_dmc.PowerSwitchDmc(device_name="fx-emu")

    def test_power_switch_dmc_is_a_power_switch(self) -> None:
        """Test case to make sure PowerSwitchDmc is PowerSwitch."""
        self.assertIsInstance(
            self.power_switch_dmc_obj, power_switch.PowerSwitch
        )

    @mock.patch.object(
        power_switch_dmc.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True,
    )
    def test_power_off(self, mock_subprocess_check_output: mock.Mock) -> None:
        """Test case for PowerSwitchDmc.power_off() success case."""
        self.power_switch_dmc_obj.power_off()
        mock_subprocess_check_output.assert_called_once()

    @parameterized.expand(
        [
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="some command",
                        output="command output",
                        stderr="command error",
                    ),
                },
            ),
            (
                {
                    "label": "TimeoutExpired",
                    "side_effect": subprocess.TimeoutExpired(
                        cmd="some command",
                        timeout=60,
                    ),
                },
            ),
            (
                {
                    "label": "RuntimeError",
                    "side_effect": RuntimeError("command failed"),
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        power_switch_dmc.subprocess,
        "check_output",
        side_effect=subprocess.CalledProcessError(
            returncode=1, cmd="some command"
        ),
        autospec=True,
    )
    def test_power_off_error(
        self,
        parameterized_dict: dict[str, Any],
        mock_subprocess_check_output: mock.Mock,
    ) -> None:
        """Test case for PowerSwitchDmc.power_off() error case."""
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        with self.assertRaises(power_switch.PowerSwitchError):
            self.power_switch_dmc_obj.power_off()

        mock_subprocess_check_output.assert_called_once()

    @mock.patch.object(
        power_switch_dmc.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True,
    )
    def test_power_on(self, mock_subprocess_check_output: mock.Mock) -> None:
        """Test case for PowerSwitchDmc.power_on() success case."""
        self.power_switch_dmc_obj.power_on()
        mock_subprocess_check_output.assert_called_once()

    @parameterized.expand(
        [
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="some command",
                        output="command output",
                        stderr="command error",
                    ),
                },
            ),
            (
                {
                    "label": "TimeoutExpired",
                    "side_effect": subprocess.TimeoutExpired(
                        cmd="some command",
                        timeout=60,
                    ),
                },
            ),
            (
                {
                    "label": "RuntimeError",
                    "side_effect": RuntimeError("command failed"),
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(
        power_switch_dmc.subprocess,
        "check_output",
        side_effect=subprocess.CalledProcessError(
            returncode=1, cmd="some command"
        ),
        autospec=True,
    )
    def test_power_on_error(
        self,
        parameterized_dict: dict[str, Any],
        mock_subprocess_check_output: mock.Mock,
    ) -> None:
        """Test case for PowerSwitchDmc.power_on() error case."""
        mock_subprocess_check_output.side_effect = parameterized_dict[
            "side_effect"
        ]

        with self.assertRaises(power_switch.PowerSwitchError):
            self.power_switch_dmc_obj.power_on()

        mock_subprocess_check_output.assert_called_once()
