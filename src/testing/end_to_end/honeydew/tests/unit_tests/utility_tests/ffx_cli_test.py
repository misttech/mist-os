#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.utils.ffx_cli.py."""

import subprocess
from typing import Any, Dict, List
import unittest
from unittest import mock

from honeydew import custom_types
from honeydew import errors
from honeydew.utils import ffx_cli
from parameterized import parameterized

_SSH_ADDRESS = "fe80::3804:df7d:daa8:ce6c"
_SSH_ADDRESS_SCOPE = "qemu"
_SSH_PORT = 8022
_TARGET_SSH_ADDRESS = custom_types.TargetSshAddress(
    ip=f"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}", port=_SSH_PORT)

_FFX_TARGET_SHOW_OUTPUT: bytes = (
    r'[{"title":"Target","label":"target","description":"",'
    r'"child":[{"title":"Name","label":"name","description":"Target name.",'
    r'"value":"fuchsia-emulator"},{"title":"SSH Address",'
    r'"label":"ssh_address","description":"Interface address",'
    r'"value":'
    f'"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}:{_SSH_PORT}"'
    r'}]},{"title":"Build",'
    r'"label":"build","description":"","child":[{"title":"Version",'
    r'"label":"version","description":"Build version.",'
    r'"value":"2023-02-01T17:26:40+00:00"},{"title":"Product",'
    r'"label":"product","description":"Product config.",'
    r'"value":"workstation_eng"},{"title":"Board","label":"board",'
    r'"description":"Board config.","value":"qemu-x64"},{"title":"Commit",'
    r'"label":"commit","description":"Integration Commit Date",'
    r'"value":"2023-02-01T17:26:40+00:00"}]}]').encode()

_FFX_TARGET_SHOW_JSON: List[Dict[str, Any]] = [
    {
        "title":
            "Target",
        "label":
            "target",
        "description":
            "",
        "child":
            [
                {
                    "title": "Name",
                    "label": "name",
                    "description": "Target name.",
                    "value": "fuchsia-emulator"
                }, {
                    "title": "SSH Address",
                    "label": "ssh_address",
                    "description": "Interface address",
                    "value": f"{_SSH_ADDRESS}%{_SSH_ADDRESS_SCOPE}:{_SSH_PORT}"
                }
            ]
    }, {
        "title":
            "Build",
        "label":
            "build",
        "description":
            "",
        "child":
            [
                {
                    "title": "Version",
                    "label": "version",
                    "description": "Build version.",
                    "value": "2023-02-01T17:26:40+00:00"
                }, {
                    "title": "Product",
                    "label": "product",
                    "description": "Product config.",
                    "value": "workstation_eng"
                }, {
                    "title": "Board",
                    "label": "board",
                    "description": "Board config.",
                    "value": "qemu-x64"
                }, {
                    "title": "Commit",
                    "label": "commit",
                    "description": "Integration Commit Date",
                    "value": "2023-02-01T17:26:40+00:00"
                }
            ]
    }
]


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: Dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class FfxCliTests(unittest.TestCase):
    """Unit tests for honeydew.utils.ffx_cli.py."""

    def setUp(self) -> None:
        super().setUp()

        # Start all the mock patches
        self.mock_check_output = mock.patch(
            "honeydew.utils.ffx_cli.subprocess.check_output",
            return_value=_FFX_TARGET_SHOW_OUTPUT,
            autospec=True).start()

        # Make sure all mock patches are stopped when the test is completed.
        self.addCleanup(mock.patch.stopall)

    def test_check_ffx_connection_success(self) -> None:
        """Test case for ffx_cli.check_ffx_connection() success case."""
        self.assertTrue(ffx_cli.check_ffx_connection(target="fuchsia-emulator"))

        self.mock_check_output.assert_called_once()

    def test_check_ffx_connection_failure_because_of_target_not_present(
            self) -> None:
        """Test case for ffx_cli.check_ffx_connection() failure case where
        target name passed is not same as output returned by
        ffx_cli.ffx_target_show()."""
        self.assertFalse(ffx_cli.check_ffx_connection(target="fx-emu"))

        self.mock_check_output.assert_called_once()

    def test_check_ffx_connection_failure_because_of_timeout(self) -> None:
        """Test case for ffx_cli.check_ffx_connection() failure case where
        ffx_cli.ffx_target_show() returns subprocess.TimeoutExpired."""
        self.mock_check_output.side_effect = subprocess.TimeoutExpired(
            cmd="ffx -t fuchsia-emulator target show --json", timeout=10)

        self.assertFalse(
            ffx_cli.check_ffx_connection(target="fuchsia-emulator"))

        self.mock_check_output.assert_called_once()

    @parameterized.expand(
        [
            ({
                "label": "empty_output",
                "side_effect": b"[]"
            },),
            (
                {
                    "label":
                        "CalledProcessError",
                    "side_effect":
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="ffx -t fuchsia-emulator target show")
                },),
        ],
        name_func=_custom_test_name_func)
    def test_check_ffx_connection_raises_exception(
            self, parameterized_dict) -> None:
        """Test case for ffx_cli.check_ffx_connection() case where it raises as
        exception."""
        self.mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.check_ffx_connection(target="fuchsia-emulator")

        self.mock_check_output.assert_called_once()

    def test_close(self):
        """Test case for ffx_cli.close()."""
        ffx_cli.close()

    def test_ffx_target_show_when_connected(self) -> None:
        """Verify ffx_target_show succeeds when target is connected to host."""
        result: List[Dict[str, Any]] = ffx_cli.ffx_target_show(
            target="fuchsia-emulator")
        self.assertEqual(result, _FFX_TARGET_SHOW_JSON)

        self.mock_check_output.assert_called_once()

    def test_ffx_target_show_raises_timeout_expired(self) -> None:
        """Verify ffx_target_show raising subprocess.TimeoutExpired."""
        self.mock_check_output.side_effect = subprocess.TimeoutExpired(
            cmd="some_cmd", timeout=30)

        with self.assertRaises(subprocess.TimeoutExpired):
            ffx_cli.ffx_target_show(target="fuchsia-d88c-799b-0e3a")

        self.mock_check_output.assert_called_once()

    def test_ffx_target_show_raises_ffx_command_error(self) -> None:
        """Verify ffx_target_show raising FfxCommandError."""
        self.mock_check_output.side_effect = subprocess.CalledProcessError(
            returncode=120, cmd="some_cmd")

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.ffx_target_show(target="fuchsia-d88c-799b-0e3a")

        self.mock_check_output.assert_called_once()

    def test_get_target_ssh_address(self) -> None:
        """Verify get_target_ssh_address returns SSH information of the fuchsia
        device."""
        self.assertEqual(
            ffx_cli.get_target_ssh_address(target="fuchsia-emulator"),
            _TARGET_SSH_ADDRESS)

        self.mock_check_output.assert_called_once()

    @parameterized.expand(
        [
            ({
                "label": "empty_output",
                "side_effect": b"[]"
            },),
            (
                {
                    "label":
                        "CalledProcessError",
                    "side_effect":
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="ffx -t fuchsia-emulator target show")
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_target_ssh_address_exception(self, parameterized_dict) -> None:
        """Verify get_target_ssh_address raise exception in failure cases."""
        self.mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.get_target_ssh_address(target="fuchsia-emulator")

        self.mock_check_output.assert_called_once()

    def test_get_target_type(self) -> None:
        """Verify get_target_type returns target type of fuchsia device."""
        result: str = ffx_cli.get_target_type(target="fuchsia-emulator")
        expected: str = _FFX_TARGET_SHOW_JSON[1]["child"][2]["value"]

        self.assertEqual(result, expected)

        self.mock_check_output.assert_called_once()

    @parameterized.expand(
        [
            ({
                "label": "empty_output",
                "side_effect": b"[]"
            },),
            (
                {
                    "label":
                        "CalledProcessError",
                    "side_effect":
                        subprocess.CalledProcessError(
                            returncode=1,
                            cmd="ffx -t fuchsia-emulator target show")
                },),
        ],
        name_func=_custom_test_name_func)
    def test_get_target_type_exception(self, parameterized_dict) -> None:
        """Verify get_target_type raise exception in failure cases."""
        self.mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.get_target_type(target="fuchsia-emulator")

        self.mock_check_output.assert_called_once()

    def test_setup(self):
        """Test case for ffx_cli.setup()."""
        ffx_cli.setup(logs_dir="/tmp/ffx_logs/")

        # calling setup again should fail
        with self.assertRaises(errors.FfxCommandError):
            ffx_cli.setup(logs_dir="/tmp/ffx_logs_2/")


if __name__ == "__main__":
    unittest.main()
