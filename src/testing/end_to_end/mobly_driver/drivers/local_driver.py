#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Implements BaseDriver for the local execution environment."""

from typing import Optional

import yaml

import api_mobly
import base_mobly_driver
import common


class LocalDriver(base_mobly_driver.BaseDriver):
    """Local Mobly test driver.

    This driver is used when executing Mobly tests in the local environment.
    In the local environment, it is assumed that users have full knowledge of
    the physical testbed that will be used during the Mobly test so LocalDriver
    allows for the Mobly |config_path| to be supplied directly by the user.
    """

    def __init__(
            self, config_path: str, params_path: Optional[str] = None) -> None:
        """Initializes the instance.

        Args:
          config_path: absolute path to the Mobly test config file.
          params_path: absolute path to the Mobly test params file.
        """
        super().__init__(params_path=params_path)
        self._config_path = config_path

    def generate_test_config(self) -> str:
        """Returns a Mobly test config in YAML format.

        The Mobly test config is a required input file of any Mobly tests.
        It includes information on the DUT(s) and specifies test parameters.

        Example output:
        ---
        TestBeds:
        - Name: SomeName
          Controllers:
            FuchsiaDevice:
            - name: fuchsia-1234-5678-90ab
          TestParams:
            param_1: "val_1"
            param_2: "val_2"

        If |params_path| is specified in LocalDriver(), then its content is
        added to the Mobly test config; otherwise, the test config is returned
        as-is but in YAML form.

        Returns:
          A YAML string that represents a Mobly test config.

        Raises:
          common.InvalidFormatException if the test params or tb config files
            are not valid YAML documents.
          common.DriverException if any file IO exceptions occur while reading
            user provided files.
        """
        try:
            config = common.read_yaml_from_file(self._config_path)

            if not self._params_path:
                return yaml.dump(config)

            test_params = common.read_yaml_from_file(self._params_path)
        except (IOError, OSError) as e:
            raise common.DriverException('Fail to open file: %s' % e)

        config = api_mobly.get_config_with_test_params(config, test_params)
        return yaml.dump(config)

    def teardown(self) -> None:
        pass
