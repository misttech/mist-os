#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import unittest
from unittest import mock

from parameterized import parameterized

import api_infra
import common
import driver_factory
import infra_driver
import local_driver


class DriverFactoryTest(unittest.TestCase):

    @parameterized.expand(
        [
            ('local_env', {}, local_driver.LocalDriver),
            (
                'infra_env', {
                    api_infra.BOT_ENV_TESTBED_CONFIG: 'botanist.json',
                    api_infra.BOT_ENV_TEST_OUTDIR: '/tmp/log',
                }, infra_driver.InfraDriver),
        ])
    def test_get_driver_success(self, name, test_env, expected_driver_type):
        factory = driver_factory.DriverFactory()
        with mock.patch.dict(os.environ, test_env, clear=True):
            driver = factory.get_driver()
        self.assertEqual(type(driver), expected_driver_type)

    def test_get_driver_unexpected_env_raises_exception(self):
        factory = driver_factory.DriverFactory()

        # Undefined "api_infra.BOT_ENV_TEST_OUTDIR".
        invalid_infra_env = {api_infra.BOT_ENV_TESTBED_CONFIG: 'botanist.json'}
        with mock.patch.dict(os.environ, invalid_infra_env, clear=True):
            with self.assertRaises(common.DriverException):
                driver = factory.get_driver()
