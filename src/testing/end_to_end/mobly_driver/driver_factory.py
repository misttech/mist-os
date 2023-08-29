#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Factory for initializing environment-specific MoblyDriver implementations."""

import os
from typing import Optional

import api_infra
import base_mobly_driver
import infra_driver
import local_driver
import common


class DriverFactory():
    """Factory class for BaseDriver implementations.

    This class uses the runtime environment to determine the
    environment-specific MoblyDriver to use for driving a Mobly test. This
    allows for the same Python Mobly test binary/target to be defined once and
    run in multiple execution environments.
    """

    def __init__(
            self,
            config_path: Optional[str] = None,
            params_path: Optional[str] = None,
            ffx_path: Optional[str] = None) -> None:
        """Initializes the instance.
        Args:
          config_path: absolute path to the Mobly test config file.
          params_path: absolute path to the Mobly testbed params file.
          ffx_path: absolute path to the FFX binary.
        """
        self._config_path = config_path
        self._params_path = params_path
        self._ffx_path = ffx_path

    def get_driver(self) -> base_mobly_driver.BaseDriver:
        """Returns an environment-specific Mobly Driver implementation.

        Returns:
          A base_mobly_driver.BaseDriver implementation.

        Raises:
          common.DriverException if unexpected execution environment is found.
        """
        botanist_config_path = os.getenv(api_infra.BOT_ENV_TESTBED_CONFIG)
        if not botanist_config_path:
            return local_driver.LocalDriver(
                config_path=self._config_path,
                params_path=self._params_path,
                ffx_path=self._ffx_path,
            )
        try:
            return infra_driver.InfraDriver(
                tb_json_path=os.environ[api_infra.BOT_ENV_TESTBED_CONFIG],
                log_path=os.environ[api_infra.BOT_ENV_TEST_OUTDIR],
                params_path=self._params_path,
            )
        except KeyError as e:
            raise common.DriverException(
                'Unexpected execution environment - missing env var: %s', e)
