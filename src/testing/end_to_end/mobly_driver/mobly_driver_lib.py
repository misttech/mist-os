#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains module-level functions for running Mobly Driver."""

import os
import subprocess
import time
from tempfile import NamedTemporaryFile
from typing import Optional

import base_mobly_driver


class MoblyTestTimeoutException(Exception):
    """Raised when the underlying Mobly test times out."""


class MoblyTestFailureException(Exception):
    """Raised when the underlying Mobly test returns a non-zero return code."""


def _execute_test(
        driver: base_mobly_driver.BaseDriver,
        python_path: str,
        test_path: str,
        timeout_sec: int = 0,
        test_data_path: Optional[str] = None,
        transport: Optional[str] = None,
        verbose: bool = False) -> None:
    """Executes a Mobly test with the specified Mobly Driver.

    Mobly test output is streamed to the console.

    Args:
      driver: The environment-specific Mobly driver to use for test execution.
      python_path: path to the Python runtime for to use.
      test_path: path to the Mobly test executable to run.
      timeout_sec: Number of seconds before a test is killed due to timeout.
        If set to 0, timeout is not enforced.
      test_data_path: path to directory containing test-time data
        dependencies.
      transport: host->target transport type to use.
      verbose: Whether to enable verbose output from the mobly test.

    Raises:
      MoblyTestFailureException if Mobly test returns non-zero return code.
      MoblyTestTimeoutException if Mobly test duration exceeds timeout.
    """
    test_env = os.environ.copy()
    if test_data_path:
        # Adding the test data dir to the test_env PATH enables underlying
        # Mobly tests to directly call test-time binaries without needing to
        # plumb their paths through the Mobly config.
        #
        # Order matters here as the test data deps are preferred over
        # binaries of existing names on the system.
        test_env['PATH'] = os.pathsep.join([test_data_path, test_env['PATH']])

    with NamedTemporaryFile(mode='w') as tmp_config:
        config = driver.generate_test_config(transport)
        print(f'Mobly config content:\n{config}')
        tmp_config.write(config)
        tmp_config.flush()

        cmd = [python_path, test_path, '-c', tmp_config.name]
        if (verbose):
            cmd.append('-v')
        cmd_str = ' '.join(cmd)
        print(f'Executing Mobly test via cmd:\n"$ {cmd_str}"')

        with subprocess.Popen(cmd, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, universal_newlines=True,
                              env=test_env) as proc:

            timeout_ts = time.time() + timeout_sec
            # Poll the process to stream output.
            while not timeout_sec or time.time() < timeout_ts:
                return_code = proc.poll()
                if return_code is not None:
                    if return_code == 0:
                        return
                    # TODO(fxbug.dev/119651) - differentiate between legitimate
                    # test failures vs unexpected crashes.
                    raise MoblyTestFailureException('Mobly test failed.')
                output = proc.stdout.readline()
                if output:
                    print(output.strip())
            # Mobly test timed out.
            proc.kill()
            proc.wait(timeout=10)
            raise MoblyTestTimeoutException(
                f'Mobly test timed out after {timeout_sec} seconds.')


def run(
        driver: base_mobly_driver.BaseDriver,
        python_path: str,
        test_path: str,
        timeout_sec: int = 0,
        test_data_path: Optional[str] = None,
        transport: Optional[str] = None,
        verbose: bool = False) -> None:
    """Runs the Mobly Driver which handles the lifecycle of a Mobly test.

    This method manages the lifecycle of a Mobly test's execution.
    At a high level, run() creates a Mobly config, triggers a Mobly test with
    it, and performs any necessary clean up after test execution.

    Args:
      driver: The environment-specific Mobly driver to use for test execution.
      python_path: path to the Python runtime to use for test execution.
      test_path: path to the Mobly test executable to run.
      timeout_sec: Number of seconds before a test is killed due to timeout.
          If set to 0, timeout is not enforced.
      test_data_path: path to directory containing test-time data
          dependencies.
      transport: host->target transport type to use.
      verbose: Whether to enable verbose output from the mobly test.

    Raises:
      MoblyTestFailureException if the test returns a non-zero return code.
      MoblyTestTimeoutException if the test duration exceeds specified timeout.
      ValueError if any argument is invalid.
    """
    if not driver:
        raise ValueError('|driver| must not be None.')
    if not python_path:
        raise ValueError('|python_path| must not be empty.')
    if not test_path:
        raise ValueError('|test_path| must not be empty.')
    if timeout_sec < 0:
        raise ValueError('|timeout_sec| must be a positive integer.')
    print(f'Running [{driver.__class__.__name__}]')
    try:
        _execute_test(
            python_path=python_path,
            test_path=test_path,
            driver=driver,
            timeout_sec=timeout_sec,
            test_data_path=test_data_path,
            transport=transport,
            verbose=verbose)
    finally:
        driver.teardown()
