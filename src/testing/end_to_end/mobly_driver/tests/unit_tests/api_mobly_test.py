#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's api_mobly.py."""

import unittest
import json

from tempfile import NamedTemporaryFile
from mobly import config_parser
from parameterized import parameterized

import api_mobly


class ApiMoblyTest(unittest.TestCase):
    """API Mobly tests"""

    def test_get_latest_test_output_dir_symlink_path_success(self):
        """Test case to ensure test output symlink is returned"""
        api_mobly.get_latest_test_output_dir_symlink_path('output_path', 'tb')

    @parameterized.expand(
        [
            ('Output path is empty', '', 'tb'),
            ('Testbed name is empty', 'ouput_path', ''),
        ])
    def test_get_latest_test_output_dir_symlink_path_raises_exception(
            self, unused_name, output_path, tb_name):
        """Test case to ensure exception is raised on test output path errors"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_latest_test_output_dir_symlink_path(
                output_path, tb_name)

    def test_get_result_path_success(self):
        """Test case to ensure test result symlink is returned"""
        api_mobly.get_result_path('output_path', 'tb')

    @parameterized.expand(
        [
            ('Output path is empty', '', 'tb'),
            ('Testbed name is empty', 'ouput_path', ''),
        ])
    def test_get_get_result_path_raises_exception(
            self, unused_name, output_path, tb_name):
        """Test case to ensure exception is raised on test result path errors"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_result_path(output_path, tb_name)

    @parameterized.expand(
        [
            ('success_empty_controllers_empty_params', [], {}, {}),
            (
                'success_valid_controllers_valid_params',
                [{
                    'type': 'FuchsiaDevice',
                    'nodename': 'fuchsia_abcd'
                }], {
                    'param': 'value'
                },{}),
            (
                'success_multiple_controllers_same_type', [
                    {
                        'type': 'FuchsiaDevice',
                        'nodename': 'fuchsia_abcd'
                    }, {
                        'type': 'FuchsiaDevice',
                        'nodename': 'fuchsia_bcde'
                    }
                ], {}, {}),
            (
                'success_multiple_controllers_different_types', [
                    {
                        'type': 'FuchsiaDevice',
                        'nodename': 'fuchsia_abcd'
                    }, {
                        'type': 'AccessPoint',
                        'ip': '192.168.42.11'
                    }
                ], {}, {}),
            (
                'success_controller_with_translation_map', [
                    {
                        'type': 'FuchsiaDevice',
                        'nodename': 'fuchsia_abcd',
                        'ssh_key': 'key',
                    },
                ], {}, {
                    "nodename": "name",
                    "ssh_key": "ssh_private_key",
                }),
        ])
    def test_new_testbed_config(self, unused_name, controllers, params_dict,
                                botanist_honeydew_map):
        """Test case for new testbed config generation"""
        config_obj = api_mobly.new_testbed_config(
            'tb_name', 'log_path', controllers, params_dict,
            botanist_honeydew_map)

        with NamedTemporaryFile(mode='w') as config_fh:
            config_fh.write(json.dumps(config_obj))
            config_fh.flush()
            # Assert that no exceptions are raised.
            config_parser.load_test_config_file(config_fh.name)

    @parameterized.expand(
        [
            # (name, config_dict, params_dict, expected_config_dict)
            (
                'Single-testbed config with params', {
                    'TestBeds': [{
                        'Name': 'tb_1'
                    }]
                }, {
                    'param_1': 'val_1'
                }, {
                    'TestBeds':
                        [{
                            'Name': 'tb_1',
                            'TestParams': {
                                'param_1': 'val_1'
                            }
                        }]
                }),
            (
                'Multi-testbed config with params', {
                    'TestBeds': [{
                        'Name': 'tb_1'
                    }, {
                        'Name': 'tb_2'
                    }]
                }, {
                    'param_1': 'val_1'
                }, {
                    'TestBeds':
                        [
                            {
                                'Name': 'tb_1',
                                'TestParams': {
                                    'param_1': 'val_1'
                                }
                            }, {
                                'Name': 'tb_2',
                                'TestParams': {
                                    'param_1': 'val_1'
                                }
                            }
                        ]
                }),
            (
                'Empty params', {
                    'TestBeds': [{
                        'Name': 'tb_1'
                    }]
                }, {}, {
                    'TestBeds': [{
                        'Name': 'tb_1',
                        'TestParams': {}
                    }]
                }),
            (
                'None params', {
                    'TestBeds': [{
                        'Name': 'tb_1'
                    }]
                }, None, {
                    'TestBeds': [{
                        'Name': 'tb_1',
                        'TestParams': None
                    }]
                }),
        ])
    def test_get_config_with_test_params_success(
            self, unused_name, config_dict, params_dict, expected_config_dict):
        """Test case for testbed config with params"""
        ret = api_mobly.get_config_with_test_params(config_dict, params_dict)
        self.assertDictEqual(ret, expected_config_dict)

    @parameterized.expand([
        ('Config is None', None),
        ('Config is empty', {}),
    ])
    def test_get_config_with_test_params_raises_exception(
            self, unused_name, config_dict):
        """Test case for exceptions in testbed config generation"""
        with self.assertRaises(api_mobly.ApiException):
            api_mobly.get_config_with_test_params(config_dict, None)
