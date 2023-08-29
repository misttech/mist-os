#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Implements BaseDriver for the local execution environment."""

import json
import subprocess
import tempfile
from typing import Any, Dict, List, Optional

import yaml

import api_infra
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
            self,
            config_path: Optional[str] = None,
            params_path: Optional[str] = None,
            ffx_path: Optional[str] = None) -> None:
        """Initializes the instance.

        Args:
          config_path: absolute path to the Mobly test config file.
          params_path: absolute path to the Mobly test params file.
          ffx_path: absolute path to the FFX binary.
        """
        super().__init__(params_path=params_path)
        self._config_path = config_path
        self._ffx_path = ffx_path

    def _list_local_fuchsia_targets(self) -> List[str]:
        """Returns Fuchsia target names detected on the host.

        Returns:
          A list of Fuchsia target names.

        Raises:
          common.InvalidFormatException if unable to extract target names from
            device discovery output.
          common.DriverException if device discovery command fails or no devices
            detected.
        """
        # Run `ffx` command in isolation mode.
        with tempfile.TemporaryDirectory() as iso_dir:
            try:
                cmd = [
                    self._ffx_path, '--isolate-dir', iso_dir, '--machine',
                    'json', 'target', 'list'
                ]
                output = subprocess.check_output(cmd, timeout=5).decode()
            except (subprocess.CalledProcessError,
                    subprocess.TimeoutExpired) as e:
                raise common.DriverException(
                    f'Failed to enumerate devices via {cmd}: {e}')

        target_names: List[str] = []
        try:
            target_names = [t['nodename'] for t in json.loads(output)]
        except json.JSONDecodeError as e:
            raise common.InvalidFormatException(
                f'Failed to decode output from {cmd}: {e}')
        except KeyError as e:
            raise common.InvalidFormatException(f'Unexpected FFX output: {e}')

        if len(target_names) == 0:
            # Raise exception here because any meaningful Mobly test should run
            # against at least one Fuchsia target.
            raise common.DriverException('No devices found.')

        print(f'Found {len(target_names)} Fuchsia target(s): {target_names}')
        return target_names

    def _generate_config_from_env(self) -> api_mobly.MoblyConfigComponent:
        """Returns Mobly device config generated from local environment.

        Best effort config generation based on Fuchsia device discovery on local
        host.

        Returns:
          A list of Fuchsia target names.

        Raises:
          common.InvalidFormatException if unable to extract target names from
            device discovery output.
          common.DriverException if device discovery command fails or no devices
            detected.
        """
        mobly_controllers: List[str] = []
        for target in self._list_local_fuchsia_targets():
            mobly_controllers.append(
                {
                    'type': api_infra.FUCHSIA_DEVICE,
                    'name': target,
                    # Assume connected devices are provisioned with default
                    # Fuchsia.git SSH credentials.
                    'ssh_private_key': '~/.ssh/fuchsia_ed25519'
                })

        return api_mobly.new_testbed_config(
            testbed_name='GeneratedLocalTestbed',
            log_path='/tmp/logs/mobly',
            mobly_controllers=mobly_controllers,
            test_params_dict={},
            botanist_honeydew_map={})

    def generate_test_config(self, transport: Optional[str] = None) -> str:
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

        Args:
          transport: host->device transport type to use.

        Returns:
          A YAML string that represents a Mobly test config.

        Raises:
          common.InvalidFormatException if the test params or tb config files
            are not valid YAML documents.
          common.DriverException if Mobly config generation fails.
        """
        config: Dict[str, Any] = {}
        if self._config_path is None:
            print('Generating Mobly config from environment...')
            print('(To override, provide path to YAML via `config_yaml_path`)')
            try:
                config = self._generate_config_from_env()
            except (common.DriverException, common.InvalidFormatException) as e:
                raise common.DriverException(
                    f'Local config generation failed: {e}')
        else:
            print('Using provided Mobly config YAML...')
            try:
                config = common.read_yaml_from_file(self._config_path)
            except (IOError, OSError) as e:
                raise common.DriverException(f'Local config parse failed: {e}')

        if self._params_path:
            test_params = common.read_yaml_from_file(self._params_path)
            config = api_mobly.get_config_with_test_params(config, test_params)

        if transport:
            api_mobly.set_transport_in_config(config, transport)

        return yaml.dump(config)

    def teardown(self) -> None:
        pass
