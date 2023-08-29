#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's local_driver.py."""

import subprocess
import unittest
from unittest.mock import ANY, patch

from parameterized import parameterized

import common
import local_driver


class LocalDriverTest(unittest.TestCase):
    """Local Driver tests"""

    @patch('builtins.print')
    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_from_file_with_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        """Test case for successful config generation from file"""
        driver = local_driver.LocalDriver(
            config_path='config/path', params_path='params/path')
        ret = driver.generate_test_config()

        mock_get_config.assert_called_once()
        self.assertEqual(mock_read_yaml.call_count, 2)
        self.assertEqual(ret, 'yaml_str')

    @patch('builtins.print')
    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_from_file_without_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        """Test case for successful config without params generation"""
        driver = local_driver.LocalDriver(config_path='config/path')
        ret = driver.generate_test_config()

        mock_get_config.assert_not_called()
        mock_read_yaml.assert_called_once()
        self.assertEqual(ret, 'yaml_str')

    @patch('builtins.print')
    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    @patch('api_mobly.set_transport_in_config')
    def test_generate_test_config_from_file_with_transport_success(
            self, mock_set_transport, *unused_args):
        """Test case for successful config without params generation"""
        transport_name = 'transport'
        driver = local_driver.LocalDriver(config_path='config/path')
        ret = driver.generate_test_config(transport=transport_name)

        mock_set_transport.assert_called_with(ANY, transport_name)

    @patch('builtins.print')
    @patch(
        'common.read_yaml_from_file', side_effect=common.InvalidFormatException)
    def test_generate_test_config_from_file_invalid_yaml_content_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid YAML content"""
        driver = local_driver.LocalDriver(config_path='config/path')
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch('builtins.print')
    @patch('common.read_yaml_from_file', side_effect=OSError)
    def test_generate_test_config_from_file_invalid_path_raises_exception(
            self, *unused_args):
        """Test case for exception being raised for invalid path"""
        driver = local_driver.LocalDriver(config_path='/does/not/exist')
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()

    @patch('builtins.print')
    @patch('yaml.dump', return_value='yaml_str')
    @patch(
        'subprocess.check_output',
        return_value=b'[{"nodename": "dut_1"}, {"nodename": "dut_2"}]',
        autospec=True)
    @patch('api_mobly.new_testbed_config', autospec=True)
    def test_generate_test_config_from_env_success(
            self, mock_new_tb_config, mock_check_output, *unused_args):
        """Test case for successful env config generation"""
        driver = local_driver.LocalDriver()
        ret = driver.generate_test_config()

        mock_new_tb_config.assert_called_once()
        controllers = mock_new_tb_config.call_args.kwargs['mobly_controllers']
        self.assertEqual(2, len(controllers))
        self.assertEqual([c['name'] for c in controllers], ['dut_1', 'dut_2'])
        self.assertEqual(ret, 'yaml_str')

    @patch('builtins.print')
    @patch(
        'subprocess.check_output',
        side_effect=subprocess.CalledProcessError(
            returncode=1, cmd=[], stderr='some error'),
        autospec=True)
    def test_generate_test_config_from_env_discovery_failure_raises_exception(
            self, mock_check_output, *unused_args):
        """Test case for exception being raised from discovery failure"""
        driver = local_driver.LocalDriver()
        with self.assertRaises(common.DriverException):
            ret = driver.generate_test_config()

    @parameterized.expand(
        [
            ('Invalid JSON str', b''),
            ('No devices JSON str', b'[]'),
            ('Empty device JSON str', b'[{}]'),
        ])
    @patch('builtins.print')
    @patch('subprocess.check_output', autospec=True)
    def test_generate_test_config_from_env_discovery_output_raises_exception(
            self, unused_name, discovery_output, mock_check_output,
            unused_print):
        """Test case for exception being raised from invalid discovery output"""
        mock_check_output.return_value = discovery_output
        driver = local_driver.LocalDriver()
        with self.assertRaises(common.DriverException):
            ret = driver.generate_test_config()
