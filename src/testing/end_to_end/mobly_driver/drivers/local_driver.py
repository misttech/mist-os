#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from typing import Optional

import yaml

import api_mobly
import base_mobly_driver


class InvalidFormatException(Exception):
    """Raised when the file input provided to the Mobly Driver is invalid."""


class DriverException(Exception):
    """Raised when the Mobly Driver encounters an unexpected error."""


def _read_yaml_from_file(filepath: str):
    """Returns a Mobly test config in YAML format.

    Args:
      filepath: absolute path to file to read YAML from.

    Returns:
      A Python object that represents the file's YAML content.

    Raises:
      InvalidFormatException if the test params or tb config files are not valid YAML documents.
    """
    with open(filepath) as c:
        try:
            return yaml.load(c, Loader=yaml.SafeLoader)
        except yaml.YAMLError as e:
            raise InvalidFormatException(
                'Failed to parse local Mobly config YAML: %s' % e)


class LocalDriver(base_mobly_driver.BaseDriver):

    def __init__(
            self, config_path: str, params_path: Optional[str] = None) -> None:
        """Initializes the instance.

        Args:
          config_path: absolute path to the Mobly testbed config file.
          params_path: absolute path to the Mobly test params file.
        """
        super().__init__(params_path=params_path)
        self._config_path = config_path

    def generate_test_config(self) -> str:
        """Returns a Mobly test config in YAML format.

        The Mobly test config is a required input file of any Mobly tests.
        It includes information on the devices-under-test and specifies test parameters.

        Example output:
        ---
        TestBeds:
        - Name: SomeName
          Controllers:
            FuchsiaDevice:
            - nodename: fuchsia-1234-5678-90ab
          TestParams:
            param_1: "val_1"
            param_2: "val_2"

        If |params_path| is specified in LocalDriver(), then its content is added to the
        Mobly test config; otherwise, the test config is returned as-is but in YAML form.

        Returns:
          A YAML string that represents a Mobly test config.

        Raises:
          InvalidFormatException if the test params or tb config files are not valid YAML documents.
          DriverException if any file IO exceptions occur while reading user provided files.
        """
        try:
            config = _read_yaml_from_file(self._config_path)

            if not self._params_path:
                return yaml.dump(config)

            test_params = _read_yaml_from_file(self._params_path)
        except (IOError, OSError) as e:
            raise DriverException('Fail to open file: %s' % e)

        config = api_mobly.get_config_with_test_params(config, test_params)
        return yaml.dump(config)

    def teardown(self) -> None:
        pass
