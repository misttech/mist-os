#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
from unittest import mock
from unittest.mock import patch

import common
import local_driver


class LocalDriverTest(unittest.TestCase):

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_with_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        d = local_driver.LocalDriver(
            config_path='config/path', params_path='params/path')
        ret = d.generate_test_config()

        mock_get_config.assert_called_once()
        self.assertEqual(mock_read_yaml.call_count, 2)
        self.assertEqual(ret, 'yaml_str')

    @patch('yaml.dump', return_value='yaml_str')
    @patch('common.read_yaml_from_file')
    @patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_without_params_success(
            self, mock_get_config, mock_read_yaml, *unused_args):
        d = local_driver.LocalDriver(config_path='config/path')
        ret = d.generate_test_config()

        mock_get_config.assert_not_called()
        mock_read_yaml.assert_called_once()
        self.assertEqual(ret, 'yaml_str')

    @patch(
        'common.read_yaml_from_file', side_effect=common.InvalidFormatException)
    def test_generate_test_config_invalid_yaml_content_raises_exception(
            self, *unused_args):
        d = local_driver.LocalDriver(config_path='config/path')
        with self.assertRaises(common.InvalidFormatException):
            d.generate_test_config()

    @patch('common.read_yaml_from_file', side_effect=OSError)
    def test_generate_test_config_invalid_path_raises_exception(
            self, *unused_args):
        d = local_driver.LocalDriver(config_path='/does/not/exist')
        with self.assertRaises(common.DriverException):
            d.generate_test_config()
