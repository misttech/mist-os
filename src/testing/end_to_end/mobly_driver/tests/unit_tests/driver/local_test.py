#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for mobly_driver/driver/local.py."""

import ipaddress
import unittest
from typing import Any
from unittest.mock import patch

from mobly_driver.api import api_ffx
from mobly_driver.driver import common, local

from ipaddress import ip_address
from parameterized import parameterized


class LocalDriverTest(unittest.TestCase):
    """Local Driver tests"""

    @patch("builtins.print")
    @patch("yaml.dump", return_value="yaml_str")
    @patch("mobly_driver.driver.common.read_yaml_from_file")
    @patch("mobly_driver.api.api_mobly.get_config_with_test_params")
    def test_generate_test_config_from_file_with_params_success(
        self, mock_get_config: Any, mock_read_yaml: Any, *unused_args: Any
    ) -> None:
        """Test case for successful config generation from file"""
        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            config_path="config/path",
            params_path="params/path",
        )
        ret = driver.generate_test_config()

        mock_get_config.assert_called_once()
        self.assertEqual(mock_read_yaml.call_count, 2)
        self.assertEqual(ret, "yaml_str")

    @patch("builtins.print")
    @patch("yaml.dump", return_value="yaml_str")
    @patch("mobly_driver.driver.common.read_yaml_from_file")
    @patch("mobly_driver.api.api_mobly.get_config_with_test_params")
    def test_generate_test_config_from_file_without_params_success(
        self, mock_get_config: Any, mock_read_yaml: Any, *unused_args: Any
    ) -> None:
        """Test case for successful config without params generation"""
        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            config_path="config/path",
        )
        ret = driver.generate_test_config()

        mock_get_config.assert_not_called()
        mock_read_yaml.assert_called_once()
        self.assertEqual(ret, "yaml_str")

    @patch("builtins.print")
    @patch(
        "mobly_driver.driver.common.read_yaml_from_file",
        side_effect=common.InvalidFormatException,
    )
    def test_generate_test_config_from_file_invalid_yaml_content_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised on invalid YAML content"""
        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            config_path="config/path",
        )
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch("builtins.print")
    @patch(
        "mobly_driver.driver.common.read_yaml_from_file", side_effect=OSError
    )
    def test_generate_test_config_from_file_invalid_path_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised for invalid path"""
        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            config_path="/does/not/exist",
        )
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch("builtins.print")
    @patch("yaml.dump", return_value="yaml_str")
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.get_target_ssh_address",
        autospec=True,
        return_value=api_ffx.TargetSshAddress(
            ip=ipaddress.ip_address("::1"), port=8022
        ),
    )
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.target_list",
        autospec=True,
        return_value=api_ffx.TargetListResult(
            all_nodes=["dut_1", "dut_2"], default_nodes=[]
        ),
    )
    @patch("mobly_driver.api.api_mobly.new_testbed_config", autospec=True)
    def test_generate_test_config_from_env_success(
        self,
        mock_new_tb_config: Any,
        mock_ffx_target_list: Any,
        mock_ffx_target_ssh_address: Any,
        *unused_args: Any,
    ) -> None:
        """Test case for successful env config generation"""
        driver = local.LocalDriver(
            ffx_path="ffx/path", transport="transport", log_path="log/path"
        )
        ret = driver.generate_test_config()

        mock_new_tb_config.assert_called_once()
        controllers = mock_new_tb_config.call_args.kwargs["mobly_controllers"]
        self.assertEqual(2, len(controllers))
        self.assertEqual([c["name"] for c in controllers], ["dut_1", "dut_2"])
        self.assertEqual(ret, "yaml_str")

        mock_ffx_target_list.assert_called()
        mock_ffx_target_ssh_address.assert_called()

    @parameterized.expand(  # type: ignore[misc]
        [
            (
                "default_nodes exist, prefer all_nodes",
                ["dut_1"],
            ),
            ("default_nodes empty, prefer all_nodes", []),
        ]
    )
    @patch("builtins.print")
    @patch("yaml.dump", return_value="yaml_str")
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.get_target_ssh_address",
        autospec=True,
        return_value=api_ffx.TargetSshAddress(
            ip=ipaddress.ip_address("::1"), port=8022
        ),
    )
    @patch("mobly_driver.api.api_ffx.FfxClient.target_list", autospec=True)
    @patch("mobly_driver.api.api_mobly.new_testbed_config", autospec=True)
    def test_multi_device_config_generation(
        self,
        unused_name: str,
        default_nodes: list[str],
        mock_new_tb_config: Any,
        mock_ffx_target_list: Any,
        mock_ffx_target_ssh_address: Any,
        *unused_args: Any,
    ) -> None:
        """Test case for multi-device config generation."""
        mock_ffx_target_list.return_value = api_ffx.TargetListResult(
            all_nodes=["dut_1", "dut_2"], default_nodes=default_nodes
        )

        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            multi_device=True,
        )
        ret = driver.generate_test_config()

        mock_new_tb_config.assert_called()
        controllers = mock_new_tb_config.call_args.kwargs["mobly_controllers"]
        self.assertEqual([c["name"] for c in controllers], ["dut_1", "dut_2"])
        self.assertEqual(ret, "yaml_str")

        mock_ffx_target_list.assert_called()
        mock_ffx_target_ssh_address.assert_called()

    @parameterized.expand(  # type: ignore[misc]
        [
            ("default_nodes exist, prefer default_nodes", ["dut_1"], ["dut_1"]),
            ("default_nodes empty, prefer all_nodes", [], ["dut_1", "dut_2"]),
        ]
    )
    @patch("builtins.print")
    @patch("yaml.dump", return_value="yaml_str")
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.get_target_ssh_address",
        autospec=True,
        return_value=api_ffx.TargetSshAddress(
            ip=ipaddress.ip_address("::1"), port=8022
        ),
    )
    @patch("mobly_driver.api.api_ffx.FfxClient.target_list", autospec=True)
    @patch("mobly_driver.api.api_mobly.new_testbed_config", autospec=True)
    def test_single_device_config_generation(
        self,
        unused_name: str,
        default_nodes: list[str],
        want_nodes: list[str],
        mock_new_tb_config: Any,
        mock_ffx_target_list: Any,
        mock_ffx_target_ssh_address: Any,
        *unused_args: Any,
    ) -> None:
        """Test case for single-device config generation."""
        mock_ffx_target_list.return_value = api_ffx.TargetListResult(
            all_nodes=["dut_1", "dut_2"], default_nodes=default_nodes
        )

        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
            multi_device=False,
        )
        ret = driver.generate_test_config()

        mock_new_tb_config.assert_called()
        controllers = mock_new_tb_config.call_args.kwargs["mobly_controllers"]
        self.assertEqual([c["name"] for c in controllers], want_nodes)
        self.assertEqual(ret, "yaml_str")

        mock_ffx_target_list.assert_called()
        mock_ffx_target_ssh_address.assert_called()

    @patch("builtins.print")
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.target_list",
        autospec=True,
        return_value=api_ffx.TargetListResult(
            all_nodes=[],
            default_nodes=[],
        ),
    )
    def test_config_generation_no_devices_raises_exception(
        self, mock_check_output: Any, *unused_args: Any
    ) -> None:
        """Test case for exception being raised when no devices are found"""
        driver = local.LocalDriver(
            ffx_path="ffx/path",
            transport="transport",
            log_path="log/path",
        )
        with self.assertRaises(common.DriverException):
            ret = driver.generate_test_config()

    @patch("builtins.print")
    @patch(
        "mobly_driver.api.api_ffx.FfxClient.target_list",
        side_effect=api_ffx.CommandException(),
        autospec=True,
    )
    def test_generate_test_config_from_env_discovery_failure_raises_exception(
        self, mock_check_output: Any, *unused_args: Any
    ) -> None:
        """Test case for exception being raised from discovery failure"""
        driver = local.LocalDriver(
            ffx_path="ffx/path", transport="transport", log_path="log/path"
        )
        with self.assertRaises(common.DriverException):
            ret = driver.generate_test_config()

    @parameterized.expand(  # type: ignore[misc]
        [
            ("Invalid JSON str", b""),
            ("No devices JSON str", b"[]"),
            ("Empty device JSON str", b"[{}]"),
        ]
    )
    @patch("builtins.print")
    @patch("subprocess.check_output", autospec=True)
    def test_generate_test_config_from_env_discovery_output_raises_exception(
        self,
        unused_name: str,
        discovery_output: bytes,
        mock_check_output: Any,
        unused_print: Any,
    ) -> None:
        """Test case for exception being raised from invalid discovery output"""
        mock_check_output.return_value = discovery_output
        driver = local.LocalDriver(
            ffx_path="ffx/path", transport="transport", log_path="log/path"
        )
        with self.assertRaises(common.DriverException):
            ret = driver.generate_test_config()
