#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.ssh.py."""

import ipaddress
import subprocess
import unittest
from typing import Any
from unittest import mock

from parameterized import parameterized

from honeydew import custom_types, errors
from honeydew.transports import ffx, ssh

_IPV4: str = "11.22.33.44"
_IPV4_OBJ: ipaddress.IPv4Address = ipaddress.IPv4Address(_IPV4)

_SSH_PORT: int = 22

_IPV4_NO_PORT: custom_types.IpPort = custom_types.IpPort(
    ip=_IPV4_OBJ, port=None
)

_SSH_USER: str = "root"
_SSH_PRIVATE_KEY: str = "/tmp/.ssh/pkey"
_DEVICE_NAME: str = "fuchsia-emulator"

_INPUT_ARGS: dict[str, Any] = {
    "device_name": _DEVICE_NAME,
    "device_ssh_ipv4_no_port": _IPV4_NO_PORT,
    "ssh_private_key": _SSH_PRIVATE_KEY,
    "ssh_user": _SSH_USER,
}

_MOCK_ARGS: dict[str, Any] = {
    "target_ssh_address": custom_types.TargetSshAddress(
        ip=_IPV4_OBJ, port=_SSH_PORT
    ),
}


def _custom_test_name_func(testcase_func, _, param) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param.args[0]
    test_label: str = parameterized.to_safe_name(params_dict["label"])

    return f"{test_func_name}_with_{test_label}"


class SshTests(unittest.TestCase):
    """Unit tests for honeydew.transports.ssh.py."""

    def setUp(self) -> None:
        super().setUp()

        self.ffx_obj = mock.MagicMock(spec=ffx.FFX)

        self.ssh_obj_wo_ip = ssh.SSH(
            device_name=_INPUT_ARGS["device_name"],
            private_key=_INPUT_ARGS["ssh_private_key"],
            ffx_transport=self.ffx_obj,
        )

        self.ssh_obj_with_ip = ssh.SSH(
            device_name=_INPUT_ARGS["device_name"],
            private_key=_INPUT_ARGS["ssh_private_key"],
            ip_port=_INPUT_ARGS["device_ssh_ipv4_no_port"],
            ffx_transport=self.ffx_obj,
        )

    @mock.patch("time.sleep", autospec=True)
    @mock.patch.object(
        ssh.SSH,
        "run",
        side_effect=[subprocess.CalledProcessError, b"some output"],
        autospec=True,
    )
    def test_ssh_check_connection_success(
        self, mock_ssh_run, mock_sleep
    ) -> None:
        """Testcase for SSH.check_connection() success case"""
        self.ssh_obj_wo_ip.check_connection(timeout=5)

        mock_ssh_run.assert_called()
        mock_sleep.assert_called()

    @mock.patch("time.sleep", autospec=True)
    @mock.patch("time.time", side_effect=[0, 1, 2], autospec=True)
    @mock.patch.object(
        ssh.SSH, "run", side_effect=subprocess.CalledProcessError, autospec=True
    )
    def test_ssh_check_connection_fail(
        self, mock_ssh_run, mock_time, mock_sleep
    ) -> None:
        """Testcase for SSH.check_connection() failure case"""
        with self.assertRaises(errors.SshConnectionError):
            self.ssh_obj_wo_ip.check_connection(timeout=2)

        mock_ssh_run.assert_called()
        mock_time.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(
        ssh.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True,
    )
    def test_ssh_run_wo_device_ip(self, mock_check_output) -> None:
        """Testcase for SSH.run() when called using SSH object created without
        device_ip argument."""
        self.ffx_obj.get_target_ssh_address.return_value = _MOCK_ARGS[
            "target_ssh_address"
        ]
        self.assertEqual(
            self.ssh_obj_wo_ip.run(command="some_command"), "some output"
        )

        mock_check_output.assert_called()

    @mock.patch.object(
        ssh.subprocess,
        "check_output",
        return_value=b"some output",
        autospec=True,
    )
    def test_ssh_run_with_device_ip(self, mock_check_output) -> None:
        """Testcase for SSH.run() when called using SSH object created with
        device_ip argument."""
        self.assertEqual(
            self.ssh_obj_with_ip.run(command="some_command"), "some output"
        )

        mock_check_output.assert_called()

    @parameterized.expand(
        [
            (
                {
                    "label": "CalledProcessError",
                    "side_effect": subprocess.CalledProcessError(
                        returncode=1,
                        cmd="ssh fuchsia@12.34.56.78:ls",
                        output="command output",
                        stderr="command error",
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
    @mock.patch.object(ssh.subprocess, "check_output", autospec=True)
    def test_ssh_run_exception(
        self, parameterized_dict, mock_check_output
    ) -> None:
        """Testcase for SSH.run() raising errors.SSHCommandError exception"""
        self.ffx_obj.get_target_ssh_address.return_value = _MOCK_ARGS[
            "target_ssh_address"
        ]
        mock_check_output.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.SSHCommandError):
            self.ssh_obj_wo_ip.run(command="some_command")

        mock_check_output.assert_called()
