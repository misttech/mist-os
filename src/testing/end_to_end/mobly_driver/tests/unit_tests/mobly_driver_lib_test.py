#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for Mobly driver's mobly_driver_lib.py."""

import os
import unittest
from unittest import mock

from parameterized import parameterized

import mobly_driver_lib


class MoblyDriverLibTest(unittest.TestCase):
    """Mobly Driver lib tests"""

    def setUp(self):
        self.mock_tmp = mock.Mock()
        self.mock_process = mock.Mock()
        self.mock_driver = mock.Mock()
        self.mock_driver.generate_test_config.return_value = ''

    @mock.patch('builtins.print')
    @mock.patch('subprocess.Popen')
    def test_run_success(self, mock_popen, mock_print):
        """Test case to ensure run succeeds"""
        self.mock_process.stdout.readline.return_value = 'TEST_OUTPUT'
        self.mock_process.poll.side_effect = [None, 0]
        mock_popen.return_value.__enter__.return_value = self.mock_process

        mobly_driver_lib.run(self.mock_driver, '/py/path', '/test/path')

        self.mock_driver.generate_test_config.assert_called()
        self.mock_driver.teardown.assert_called()
        self.assertIn(mock.call('TEST_OUTPUT'), mock_print.call_args_list)

    @parameterized.expand(
        [
            ['invalid_driver', None, '/py/path', '/test/path', 0],
            ['invalid_python_path',
             mock.Mock(), '', '/test/path', 0],
            ['invalid_test_path',
             mock.Mock(), '/py/path', '', 0],
            ['invalid_timeout',
             mock.Mock(), '/py/path', '/test/path', -1]
        ])
    def test_run_invalid_argument_raises_exception(
            self, unused_name, driver, python_path, test_path, timeout_sec):
        """Test case to ensure exception raised on invalid args"""
        with self.assertRaises(ValueError):
            mobly_driver_lib.run(
                driver, python_path, test_path, timeout_sec=timeout_sec)

    @mock.patch('builtins.print')
    @mock.patch('subprocess.Popen')
    def test_run_mobly_test_failure_raises_exception(
            self, mock_popen, *unused_args):
        """Test case to ensure exception raised on test failure"""
        self.mock_process.poll.return_value = 1
        mock_popen.return_value.__enter__.return_value = self.mock_process

        with self.assertRaises(mobly_driver_lib.MoblyTestFailureException):
            mobly_driver_lib.run(self.mock_driver, '/py/path', '/test/path')

    @mock.patch('builtins.print')
    @mock.patch('time.time')
    @mock.patch('subprocess.Popen')
    def test_run_mobly_test_timeout_exception(
            self, mock_popen, mock_time, *unused_args):
        """Test case to ensure exception raised on test timeout"""
        mock_popen.return_value.__enter__.return_value = self.mock_process

        mock_time.side_effect = [0, 10]

        with self.assertRaises(mobly_driver_lib.MoblyTestTimeoutException):
            mobly_driver_lib.run(
                self.mock_driver, '/py/path', '/test/path', timeout_sec=5)
        self.mock_process.kill.assert_called()

    @mock.patch('builtins.print')
    @mock.patch('subprocess.Popen')
    def test_run_teardown_runs_despite_subprocess_error(
            self, mock_popen, mock_print):
        """Test case to ensure teardown always executes"""
        self.mock_process.stdout.readline.return_value = 'MOCK_FAILURE_OUTPUT'
        self.mock_process.poll.side_effect = [None, 1]
        mock_popen.return_value.__enter__.return_value = self.mock_process

        with self.assertRaises(mobly_driver_lib.MoblyTestFailureException):
            mobly_driver_lib.run(self.mock_driver, '/py/path', '/test/path')
        self.mock_driver.teardown.assert_called()
        self.assertIn(
            mock.call('MOCK_FAILURE_OUTPUT'), mock_print.call_args_list)

    @parameterized.expand([[True], [False]])
    @mock.patch('builtins.print')
    @mock.patch('subprocess.Popen')
    @mock.patch('mobly_driver_lib.NamedTemporaryFile')
    def test_run_passes_params_to_popen(
            self, verbose, mock_tempfile, mock_popen, *unused_args):
        """Test case to ensure correct params are passed to Popen"""
        tmp_path = '/tmp/path'
        py_path = '/py/path'
        test_path = '/test/path'
        self.mock_tmp.name = tmp_path
        self.mock_process.poll.side_effect = [None, 0]
        mock_tempfile.return_value.__enter__.return_value = self.mock_tmp
        mock_popen.return_value.__enter__.return_value = self.mock_process

        mobly_driver_lib.run(
            self.mock_driver, py_path, test_path, verbose=verbose)
        mock_popen.assert_called_once_with(
            [py_path, test_path, '-c', tmp_path] + (['-v'] if verbose else []),
            stdout=mock.ANY,
            stderr=mock.ANY,
            universal_newlines=mock.ANY,
            env=mock.ANY)

    @mock.patch('builtins.print')
    @mock.patch('subprocess.Popen')
    def test_run_updates_env_with_testdata_dir(self, mock_popen, *unused_args):
        """Test case to ensure env is updated when test_data_path is provided"""
        self.mock_process.stdout.readline.return_value = 'TEST_OUTPUT'
        self.mock_process.poll.side_effect = [None, 0]
        mock_popen.return_value.__enter__.return_value = self.mock_process

        env = {'PATH': '/system/path'}
        with mock.patch.dict(os.environ, env, clear=True):
            mobly_driver_lib.run(
                self.mock_driver,
                '/py/path',
                '/test/path',
                test_data_path='/test_data/path')
            mock_popen.assert_called_once_with(
                mock.ANY,
                stdout=mock.ANY,
                stderr=mock.ANY,
                universal_newlines=mock.ANY,
                env={'PATH': '/test_data/path:/system/path'})
