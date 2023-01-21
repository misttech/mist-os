#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import subprocess
import time
from tempfile import NamedTemporaryFile

import base_mobly_driver


class MoblyTestTimeoutException(Exception):
    """Raised when the underlying Mobly test times out."""


class MoblyTestFailureException(Exception):
    """Raised when the underlying Mobly test returns a a non-zero return code."""


def _execute_test(
        driver: base_mobly_driver.BaseDriver,
        python_path: str,
        test_path: str,
        timeout_sec: int = 0) -> None:
    """Executes a Mobly test with the specified |tb_config_yaml_content| as test input.

    Mobly test output is streamed to the console.

    Args:
        driver: The environment-specific Mobly driver to use for test execution.
        python_path: absolute path to the Python runtime to use for test execution.
        test_path: absolute path to the Mobly test executable to run.
        timeout_sec: Number of seconds before a test is killed due to timeout.
          If set to 0, timeout is not enforced.

    Raises:
      MoblyTestFailureException if Mobly test returns non-zero return code.
      MoblyTestTimeoutException if Mobly test duration exceeds specified timeout.
    """
    with NamedTemporaryFile(mode='w') as tmp_config:
        config = driver.generate_test_config()
        print(f'Mobly config content:\n{config}')
        tmp_config.write(config)
        tmp_config.flush()

        cmd = [python_path, test_path, '-c', tmp_config.name]
        print('Executing Mobly test via cmd:\n"$ %s"' % ' '.join(cmd))
        with subprocess.Popen(cmd, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT,
                              universal_newlines=True) as proc:

            timeout_ts = time.time() + timeout_sec
            # Poll the process to stream output.
            while not timeout_sec or time.time() < timeout_ts:
                rc = proc.poll()
                if rc is not None:
                    if rc == 0:
                        return
                    # TODO(fxbug.dev/119651) - differentiate between legitimate test failures vs
                    # unexpected crashes.
                    raise MoblyTestFailureException('Mobly test failed.')
                output = proc.stdout.readline()
                if output:
                    print(output.decode().strip())
            # Mobly test timed out.
            proc.kill()
            proc.wait(timeout=10)
            raise MoblyTestTimeoutException(
                'Mobly test timed out after %s seconds.' % timeout_sec)


def run(
        driver: base_mobly_driver.BaseDriver,
        python_path: str,
        test_path: str,
        timeout_sec: int = 0) -> None:
    """Runs the Mobly Driver which handles the lifecycle of a Mobly test.

    This method manages the lifecycle of a Mobly test's execution.
    At a high level, run() creates a Mobly config, triggers a Mobly test with it,
    and performs any necessary clean up after test execution.

    Args:
    driver: The environment-specific Mobly driver to use for test execution.
    python_path: absolute path to the Python runtime to use for test execution.
    test_path: absolute path to the Mobly test executable to run.
    timeout_sec: Number of seconds before a test is killed due to timeout.
          If set to 0, timeout is not enforced.

    Raises:
      MoblyTestFailureException if the Mobly test returns a non-zero return code.
      MoblyTestTimeoutException if Mobly test duration exceeds specified timeout.
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
            timeout_sec=timeout_sec)
    finally:
        driver.teardown()
