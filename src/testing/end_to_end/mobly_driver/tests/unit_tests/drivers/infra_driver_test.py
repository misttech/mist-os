#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's infra_driver.py."""

import unittest
from unittest.mock import ANY, call, mock_open, patch

import common
import infra_driver


class InfraMoblyDriverTest(unittest.TestCase):
    """Infra Driver tests"""

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_json_from_file')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.new_testbed_config')
    def test_generate_test_config_with_params_success(
            self, mock_new_config, mock_read_yaml, mock_read_json,
            *unused_args):
        """Test case for successful config generation"""
        driver = infra_driver.InfraDriver(
            tb_json_path='tb/json/path', params_path='params/path', log_path='')
        ret = driver.generate_test_config()

        mock_new_config.assert_called_once()
        mock_read_yaml.assert_called_once()
        mock_read_json.assert_called_once()
        self.assertEqual(ret, 'yaml_str')

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_json_from_file')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.new_testbed_config')
    def test_generate_test_config_without_params_success(
            self, mock_new_config, mock_read_yaml, mock_read_json,
            *unused_args):
        """Test case for successful config without params generation"""
        driver = infra_driver.InfraDriver(
            tb_json_path='tb/json/path', log_path='')
        ret = driver.generate_test_config()

        mock_new_config.assert_called_once()
        mock_read_yaml.assert_not_called()
        mock_read_json.assert_called_once()
        self.assertEqual(ret, 'yaml_str')

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_json_from_file')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.new_testbed_config')
    @patch('api_mobly.set_transport_in_config')
    def test_generate_test_config_with_transport_success(
            self, mock_set_transport, *unused_args):
        """Test case for successful config without params generation"""
        transport_name = 'transport'
        driver = infra_driver.InfraDriver(
            tb_json_path='tb/json/path', log_path='')
        ret = driver.generate_test_config(transport=transport_name)

        mock_set_transport.assert_called_with(ANY, transport_name)

    @patch(
        'common.read_json_from_file', side_effect=common.InvalidFormatException)
    def test_generate_test_config_invalid_json_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid JSON content"""
        driver = infra_driver.InfraDriver(
            tb_json_path='tb/json/path', log_path='')
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch(
        'common.read_yaml_from_file', side_effect=common.InvalidFormatException)
    @patch('common.read_json_from_file')
    def test_generate_test_config_invalid_yaml_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid YAML content"""
        driver = infra_driver.InfraDriver(
            tb_json_path='tb/json/path', params_path='params/path', log_path='')
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch('common.read_json_from_file', side_effect=OSError)
    def test_generate_test_config_invalid_tb_path_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid testbed JSON path"""
        driver = infra_driver.InfraDriver(
            tb_json_path='/does/not/exist', log_path='')
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch('common.read_yaml_from_file', side_effect=OSError)
    def test_generate_test_config_invalid_params_path_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid params YAML path"""
        driver = infra_driver.InfraDriver(
            tb_json_path='/does/not/exist',
            params_path='params/path',
            log_path='')
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch('api_mobly.get_result_path')
    @patch(
        'api_mobly.get_latest_test_output_dir_symlink_path',
        return_value='path/to/remove')
    @patch('api_infra.TESTPARSER_PREAMBLE', '---MOCK_PREAMBLE---')
    @patch('builtins.open', new_callable=mock_open, read_data='test_result')
    @patch('os.remove')
    @patch('builtins.print')
    def test_teardown_success(self, mock_print, mock_rm, *unused_args):
        """Test case for teardown"""
        driver = infra_driver.InfraDriver(tb_json_path='', log_path='')
        driver.teardown()

        self.assertIn(call('---MOCK_PREAMBLE---'), mock_print.call_args_list)
        self.assertIn(call('test_result'), mock_print.call_args_list)
        mock_rm.assert_called_once_with('path/to/remove')

    @patch('api_mobly.get_result_path')
    @patch('api_mobly.get_latest_test_output_dir_symlink_path')
    @patch('api_infra.TESTPARSER_PREAMBLE')
    @patch('builtins.open', side_effect=OSError)
    @patch('os.remove', side_effect=OSError)
    @patch('builtins.print')
    def test_teardown_success_without_test_results(
            self, mock_print, mock_rm, *unused_args):
        """Test case for teardown succeeding despite missing results"""
        driver = infra_driver.InfraDriver(tb_json_path='', log_path='')
        driver.teardown()

        mock_print.assert_not_called()
        mock_rm.assert_called_once()
