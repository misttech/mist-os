#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import textwrap
import unittest
from unittest import mock
from tempfile import NamedTemporaryFile

from parameterized import parameterized

import local_driver


class LocalDriverTest(unittest.TestCase):

    @mock.patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_with_params_success(self, mock_get_config):
        mock_get_config.return_value = {
            'TestBeds':
                [{
                    'Name': 'TestbedName',
                    'TestParams': {
                        'param_1': 'val_1'
                    }
                }]
        }

        config_json = textwrap.dedent(
            """\
            {
              "TestBeds":[
                "Name": "TestbedName"
              ]
            }
            """)

        yaml_output = textwrap.dedent(
            """\
            TestBeds:
            - Name: TestbedName
              TestParams:
                param_1: val_1
            """)
        with NamedTemporaryFile(mode='w') as config, NamedTemporaryFile(
                mode='r') as test_params:
            config.write(config_json)
            config.flush()

            d = local_driver.LocalDriver(
                config_path=config.name, params_path=test_params.name)
            ret = d.generate_test_config()

        # Assert YAML is returned.
        self.assertEqual(ret, yaml_output)

    @mock.patch('api_mobly.get_config_with_test_params')
    def test_generate_test_config_without_params_success(self, mock_get_config):
        config_json = textwrap.dedent(
            """\
            {
              "TestBeds":[
                "Name": "TestbedName"
              ]
            }
            """)
        yaml_output = textwrap.dedent(
            """\
            TestBeds:
            - Name: TestbedName
            """)
        with NamedTemporaryFile(mode='w') as config:
            config.write(config_json)
            config.flush()

            d = local_driver.LocalDriver(config_path=config.name)
            ret = d.generate_test_config()

        # Assert YAML is returned.
        self.assertEqual(ret, yaml_output)
        mock_get_config.assert_not_called()

    @parameterized.expand(
        [
            ('invalid_config', '@', ''),
            ('invalid_params', '', '@'),
        ])
    def test_generate_test_config_invalid_conntent_raises_exception(
            self, name, config_yaml_str, params_yaml_str):
        with NamedTemporaryFile(mode='w') as config, NamedTemporaryFile(
                mode='w') as test_params:
            config.write(config_yaml_str)
            config.flush()

            test_params.write(params_yaml_str)
            test_params.flush()

            d = local_driver.LocalDriver(
                config_path=config.name, params_path=test_params.name)
            with self.assertRaises(local_driver.InvalidFormatException):
                d.generate_test_config()

    def test_generate_test_config_invalid_paths_raises_exception(self):
        d = local_driver.LocalDriver(
            config_path='/does/not/exist', params_path='/does/not/exist')
        with self.assertRaises(local_driver.DriverException):
            d.generate_test_config()
