#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for base_mobly_driver.py."""

import os
import unittest
from unittest.mock import patch

from parameterized import parameterized

import base_mobly_driver


class BaseMoblyDriverTest(unittest.TestCase):
    """Base Mobly Driver tests"""

    @parameterized.expand(
        [
            (
                "log_path specified",
                "/user/path",
                {
                    base_mobly_driver.TEST_OUTDIR_ENV: "/env/path",
                },
                "/user/path",
            ),
            (
                "log_path not specified",
                None,
                {
                    base_mobly_driver.TEST_OUTDIR_ENV: "/env/path",
                },
                "/env/path",
            ),
        ]
    )
    @patch.multiple(base_mobly_driver.BaseDriver, __abstractmethods__=set())
    def test_init_success(
        self, unused_name, log_path, test_env, expected_log_path, *unused_args
    ):
        """Test case for initialization success"""
        with patch.dict(os.environ, test_env, clear=True):
            d = base_mobly_driver.BaseDriver(
                ffx_path="ffx_path", log_path=log_path
            )
            self.assertEqual(d._log_path, expected_log_path)

    @patch.multiple(base_mobly_driver.BaseDriver, __abstractmethods__=set())
    def test_init_invalid_environment_raises_exception(self, *unused_args):
        """Test case for initialization failure"""
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(KeyError):
                d = base_mobly_driver.BaseDriver(ffx_path="ffx_path")
