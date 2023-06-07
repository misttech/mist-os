#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's local_driver.py."""

import unittest
from unittest.mock import ANY, patch

import common
import local_driver


class LocalDriverTest(unittest.TestCase):
    """Local Driver tests"""

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_with_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        """Test case for successful config generation"""
        driver = local_driver.LocalDriver(
            config_path='config/path', params_path='params/path')
        ret = driver.generate_test_config()

        mock_get_config.assert_called_once()
        self.assertEqual(mock_read_yaml.call_count, 2)
        self.assertEqual(ret, 'yaml_str')

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_without_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        """Test case for successful config without params generation"""
        driver = local_driver.LocalDriver(config_path='config/path')
        ret = driver.generate_test_config()

        mock_get_config.assert_not_called()
        mock_read_yaml.assert_called_once()
        self.assertEqual(ret, 'yaml_str')

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    @patch('api_mobly.set_transport_in_config')
    def test_generate_test_config_with_transport_success(
            self, mock_set_transport, *unused_args):
        """Test case for successful config without params generation"""
        transport_name = 'transport'
        driver = local_driver.LocalDriver(config_path='config/path')
        ret = driver.generate_test_config(transport=transport_name)

        mock_set_transport.assert_called_with(ANY, transport_name)

    @patch(
        'common.read_yaml_from_file', side_effect=common.InvalidFormatException)
    def test_generate_test_config_invalid_yaml_content_raises_exception(
            self, *unused_args):
        """Test case for exception being raised on invalid YAML content"""
        driver = local_driver.LocalDriver(config_path='config/path')
        with self.assertRaises(common.InvalidFormatException):
            driver.generate_test_config()

    @patch('common.read_yaml_from_file', side_effect=OSError)
    def test_generate_test_config_invalid_path_raises_exception(
            self, *unused_args):
        """Test case for execption being raised for invalid path"""
        driver = local_driver.LocalDriver(config_path='/does/not/exist')
        with self.assertRaises(common.DriverException):
            driver.generate_test_config()
