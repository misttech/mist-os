#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.transports.ssh.py."""

import ipaddress
import subprocess
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

from parameterized import param, parameterized

from honeydew import errors
from honeydew.transports import ffx, ssh
from honeydew.typing import custom_types

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


def _custom_test_name_func(
    testcase_func: Callable[..., None], _: str, param_arg: param
) -> str:
    """Custom name function method."""
    test_func_name: str = testcase_func.__name__

    params_dict: dict[str, Any] = param_arg.args[0]
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
        self, mock_ssh_run: mock.Mock, mock_sleep: mock.Mock
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
        self,
        mock_ssh_run: mock.Mock,
        mock_time: mock.Mock,
        mock_sleep: mock.Mock,
    ) -> None:
        """Testcase for SSH.check_connection() failure case"""
        with self.assertRaises(errors.SshConnectionError):
            self.ssh_obj_wo_ip.check_connection(timeout=2)

        mock_ssh_run.assert_called()
        mock_time.assert_called()
        mock_sleep.assert_called()

    @mock.patch.object(subprocess, "Popen", autospec=True)
    def test_ssh_run_wo_device_ip(self, mock_popen: mock.Mock) -> None:
        """Testcase for SSH.run() when called using SSH object created without
        device_ip argument."""
        process_mock = mock.Mock()
        attrs = {
            "communicate.return_value": (b"some output", None),
            "returncode": 0,
        }
        process_mock.configure_mock(**attrs)
        mock_popen.return_value = process_mock
        self.ffx_obj.get_target_ssh_address.return_value = _MOCK_ARGS[
            "target_ssh_address"
        ]
        self.assertEqual(
            self.ssh_obj_wo_ip.run(
                command="some_command", get_ssh_addr_timeout=3
            ),
            "some output",
        )

        mock_popen.assert_called_with(
            [
                "ssh",
                "-oPasswordAuthentication=no",
                "-oStrictHostKeyChecking=no",
                "-oConnectTimeout=3",
                "-i",
                f"{_SSH_PRIVATE_KEY}",
                "-p",
                f"{_SSH_PORT}",
                f"fuchsia@{_IPV4}",
                "some_command",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.ffx_obj.get_target_ssh_address.assert_called_with(timeout=3)

    @mock.patch.object(subprocess, "Popen", autospec=True)
    def test_ssh_run_with_device_ip(self, mock_popen: mock.Mock) -> None:
        """Testcase for SSH.run() when called using SSH object created with
        device_ip argument."""
        process_mock = mock.Mock()
        attrs = {
            "communicate.return_value": (b"some output", None),
            "returncode": 0,
        }
        process_mock.configure_mock(**attrs)
        mock_popen.return_value = process_mock
        self.assertEqual(
            self.ssh_obj_with_ip.run(command="some_command"), "some output"
        )

        mock_popen.assert_called_with(
            [
                "ssh",
                "-oPasswordAuthentication=no",
                "-oStrictHostKeyChecking=no",
                "-oConnectTimeout=3",
                "-i",
                f"{_SSH_PRIVATE_KEY}",
                f"fuchsia@{_IPV4}",
                "some_command",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    @parameterized.expand(
        [
            (
                {
                    "label": "CalledProcessError",
                    "return_code": 1,
                },
            ),
            (
                {
                    "label": "RuntimeError",
                    "return_code": 0,
                    "side_effect": RuntimeError("command failed"),
                },
            ),
        ],
        name_func=_custom_test_name_func,
    )
    @mock.patch.object(subprocess, "Popen", autospec=True)
    def test_ssh_run_exception(
        self, parameterized_dict: dict[str, Any], mock_popen: mock.Mock
    ) -> None:
        """Testcase for SSH.run() raising errors.SSHCommandError exception"""
        self.ffx_obj.get_target_ssh_address.return_value = _MOCK_ARGS[
            "target_ssh_address"
        ]
        process_mock = mock.Mock()
        attrs = {
            "communicate.return_value": (b"some output", "some error"),
            "returncode": parameterized_dict["return_code"],
        }
        process_mock.configure_mock(**attrs)
        mock_popen.return_value = process_mock
        if "side_effect" in parameterized_dict:
            mock_popen.side_effect = parameterized_dict["side_effect"]

        with self.assertRaises(errors.SSHCommandError):
            self.ssh_obj_wo_ip.run(command="some_command")

        mock_popen.assert_called()

    @mock.patch.object(subprocess, "Popen", autospec=True)
    def test_ssh_popen(self, mock_popen: mock.Mock) -> None:
        """Testcase for SSH.popen()"""
        self.assertEqual(
            self.ssh_obj_with_ip.popen("some_command"), mock_popen.return_value
        )
        mock_popen.assert_called()

    def test_ssh_ip_port(self) -> None:
        """Testcase for SSH.target_address"""
        self.ffx_obj.get_target_ssh_address.return_value = _MOCK_ARGS[
            "target_ssh_address"
        ]
        self.assertEqual(
            self.ssh_obj_wo_ip.get_target_address(),
            _MOCK_ARGS["target_ssh_address"],
        )
        self.ffx_obj.get_target_ssh_address.assert_called_with(timeout=10)
        address = self.ssh_obj_with_ip.get_target_address()
        self.assertEqual(address.ip, _INPUT_ARGS["device_ssh_ipv4_no_port"].ip)
        self.assertEqual(
            address.port, _INPUT_ARGS["device_ssh_ipv4_no_port"].port
        )
