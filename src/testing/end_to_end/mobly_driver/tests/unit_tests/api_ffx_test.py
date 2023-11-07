#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's api_ffx.py."""

import subprocess
import unittest
from unittest.mock import patch

from parameterized import parameterized

import api_ffx


class FfxClientTest(unittest.TestCase):
    """Tests for api_ffx.FfxClient"""

    def setUp(self) -> None:
        super().setUp()
        self.client = api_ffx.FfxClient("some_ffx_path")

    @parameterized.expand(
        [
            (
                "No default nodes",
                b'[{"nodename": "dut", "is_default": false}]',
                ["dut"],
                [],
            ),
            (
                "1 default node",
                b'[{"nodename": "dut", "is_default": true}]',
                ["dut"],
                ["dut"],
            ),
        ]
    )
    @patch("subprocess.check_output", autospec=True)
    def test_target_list_success(
        self,
        unused_name,
        target_list_output,
        want_all_nodes,
        want_default_nodes,
        mock_check_output,
    ):
        """Test case for target_list() returning expected results"""
        mock_check_output.return_value = target_list_output
        res = self.client.target_list(isolate_dir=None)
        self.assertEqual(res.all_nodes, want_all_nodes)
        self.assertEqual(res.default_nodes, want_default_nodes)

    @parameterized.expand(
        [
            (
                "timeout",
                subprocess.TimeoutExpired(cmd="", timeout=-1),
            ),
            (
                "failure",
                subprocess.CalledProcessError(returncode=1, cmd=[], stderr=""),
            ),
        ]
    )
    @patch("subprocess.check_output", autospec=True)
    def test_target_list_command_failure_raises_exception(
        self, unused_name, mock_exception, mock_check_output
    ):
        """Test case for exception being raised from subprocess failure"""
        mock_check_output.side_effect = mock_exception
        with self.assertRaises(api_ffx.CommandException):
            self.client.target_list(isolate_dir=None)

    @patch(
        "subprocess.check_output",
        autospec=True,
        return_value=b'[{"nodename": "dut", "is_default": false}]',
    )
    def test_target_list_with_isolated_dir(self, mock_check_output):
        """Test case for isolated dir being included in ffx command"""
        self.client.target_list(isolate_dir="some_isolate_dir_path")
        check_output_args = mock_check_output.call_args.args[0]
        self.assertIn("some_isolate_dir_path", check_output_args)

    @parameterized.expand(
        [
            ("Invalid JSON str", b""),
            ("Empty device JSON str", b"[{}]"),
        ]
    )
    @patch("subprocess.check_output", autospec=True)
    def test_target_list_invalid_output_raises_exception(
        self, unused_name, target_list_output, mock_check_output
    ):
        """Test case for exception being raised from invalid discovery output"""
        mock_check_output.return_value = target_list_output
        with self.assertRaises(api_ffx.OutputFormatException):
            self.client.target_list(isolate_dir=None)
