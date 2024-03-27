#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for mobly_driver/driver/infra.py."""

import unittest
from unittest.mock import ANY, call, mock_open, patch
from typing import Any

from mobly_driver.driver import common, infra


class InfraMoblyDriverTest(unittest.TestCase):
    """Infra Driver tests"""

    @patch("yaml.dump", return_value="yaml_str")
    @patch("mobly_driver.driver.common.read_json_from_file")
    @patch("mobly_driver.driver.common.read_yaml_from_file")
    @patch("mobly_driver.api.api_mobly.new_testbed_config")
    def test_generate_test_config_with_params_success(
        self,
        mock_new_config: Any,
        mock_read_yaml: Any,
        mock_read_json: Any,
        *unused_args: Any,
    ) -> None:
        """Test case for successful config generation"""
        driver = infra.InfraDriver(
            tb_json_path="tb/json/path",
            ffx_path="ffx/path",
            transport="transport",
            params_path="params/path",
            log_path="",
        )
        ret = driver.generate_test_config()

        mock_new_config.assert_called_once()
        mock_read_yaml.assert_called_once()
        mock_read_json.assert_called_once()
        self.assertEqual(ret, "yaml_str")

    @patch("yaml.dump", return_value="yaml_str")
    @patch("mobly_driver.driver.common.read_json_from_file")
    @patch("mobly_driver.driver.common.read_yaml_from_file")
    @patch("mobly_driver.api.api_mobly.new_testbed_config")
    def test_generate_test_config_without_params_success(
        self,
        mock_new_config: Any,
        mock_read_yaml: Any,
        mock_read_json: Any,
        *unused_args: Any,
    ) -> None:
        """Test case for successful config without params generation"""
        driver = infra.InfraDriver(
            tb_json_path="tb/json/path",
            ffx_path="ffx/path",
            transport="transport",
            log_path="",
        )
        ret = driver.generate_test_config()

        mock_new_config.assert_called_once()
        mock_read_yaml.assert_not_called()
        mock_read_json.assert_called_once()
        self.assertEqual(ret, "yaml_str")

    @patch(
        "mobly_driver.driver.common.read_json_from_file",
        side_effect=common.InvalidFormatException,
    )
    def test_generate_test_config_invalid_json_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised on invalid JSON content"""
        driver = infra.InfraDriver(
            tb_json_path="tb/json/path",
            ffx_path="ffx/path",
            transport="transport",
            log_path="",
        )
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch(
        "mobly_driver.driver.common.read_yaml_from_file",
        side_effect=common.InvalidFormatException,
    )
    @patch("mobly_driver.driver.common.read_json_from_file")
    def test_generate_test_config_invalid_yaml_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised on invalid YAML content"""
        driver = infra.InfraDriver(
            tb_json_path="tb/json/path",
            ffx_path="ffx/path",
            transport="transport",
            params_path="params/path",
            log_path="",
        )
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch(
        "mobly_driver.driver.common.read_json_from_file", side_effect=OSError
    )
    def test_generate_test_config_invalid_tb_path_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised on invalid testbed JSON path"""
        driver = infra.InfraDriver(
            tb_json_path="/does/not/exist",
            ffx_path="ffx/path",
            transport="transport",
            log_path="",
        )
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch(
        "mobly_driver.driver.common.read_yaml_from_file", side_effect=OSError
    )
    def test_generate_test_config_invalid_params_path_raises_exception(
        self, *unused_args: Any
    ) -> None:
        """Test case for exception being raised on invalid params YAML path"""
        driver = infra.InfraDriver(
            tb_json_path="/does/not/exist",
            ffx_path="ffx/path",
            transport="transport",
            params_path="params/path",
            log_path="",
        )
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch("mobly_driver.api.api_mobly.get_result_path")
    @patch(
        "mobly_driver.api.api_mobly.get_latest_test_output_dir_symlink_path",
        return_value="path/to/remove",
    )
    @patch(
        "mobly_driver.api.api_infra.TESTPARSER_RESULT_HEADER",
        "---MOCK_HEADER---",
    )
    @patch("builtins.open", new_callable=mock_open, read_data="test_result")
    @patch("os.remove")
    @patch("builtins.print")
    def test_teardown_success(
        self, mock_print: Any, mock_rm: Any, *unused_args: Any
    ) -> None:
        """Test case for teardown"""
        driver = infra.InfraDriver(
            tb_json_path="",
            ffx_path="ffx/path",
            transport="transport",
            log_path="",
        )
        driver.teardown()

        self.assertIn(
            call("---MOCK_HEADER---", flush=True), mock_print.call_args_list
        )
        self.assertIn(
            call("test_result", flush=True), mock_print.call_args_list
        )
        mock_rm.assert_called_once_with("path/to/remove")

    @patch("mobly_driver.api.api_mobly.get_result_path")
    @patch("mobly_driver.api.api_mobly.get_latest_test_output_dir_symlink_path")
    @patch("mobly_driver.api.api_infra.TESTPARSER_RESULT_HEADER")
    @patch("builtins.open", side_effect=OSError)
    @patch("os.remove", side_effect=OSError)
    @patch("builtins.print")
    def test_teardown_success_without_test_results(
        self, mock_print: Any, mock_rm: Any, *unused_args: Any
    ) -> None:
        """Test case for teardown succeeding despite missing results"""
        driver = infra.InfraDriver(
            tb_json_path="",
            ffx_path="ffx/path",
            transport="transport",
            log_path="",
        )
        driver.teardown()

        mock_print.assert_not_called()
        mock_rm.assert_called_once()
