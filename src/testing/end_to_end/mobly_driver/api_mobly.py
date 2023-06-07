#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all Mobly APIs used in Mobly Driver."""

import os
import api_infra
from mobly import keys
from mobly import records
from typing import List, Dict, Any

LATEST_RES_SYMLINK_NAME: str = 'latest'

# Fuchsia-specific keys and values used in Mobly configs.
# Defined and used in
# https://osscs.corp.google.com/fuchsia/fuchsia/+/main:src/testing/end_to_end/mobly_controller/fuchsia_device.py
MOBLY_CONTROLLER_FUCHSIA_DEVICE: str = 'FuchsiaDevice'
TRANSPORT_KEY: str = 'transport'

MoblyConfigComponent = Dict[str, Any]


class ApiException(Exception):
    pass


def get_latest_test_output_dir_symlink_path(
        mobly_output_path: str, testbed_name: str) -> str:
    """Returns the absolute path to the Mobly testbed's latest output directory.

    Args:
        mobly_output_path: absolute path to Mobly's top-level output directory.
        testbed_name: Mobly testbed name that corresponds to the test output.

    Raises:
      ApiException if arguments are invalid.

    Returns:
      The absolute path to a Mobly testbed's test output directory.
    """
    if not mobly_output_path or not testbed_name:
        raise ApiException('Arguments must be non-empty.')
    return os.path.join(
        mobly_output_path, testbed_name, LATEST_RES_SYMLINK_NAME)


def get_result_path(mobly_output_path: str, testbed_name: str) -> str:
    """Returns the absolute path to the Mobly test result file.

    Args:
        mobly_output_path: absolute path to Mobly's top-level output directory.
        testbed_name: Mobly testbed name that corresponds to the test output.

    Raises:
      ApiException if arguments are invalid.

    Returns:
      The absolute path to a Mobly test result file.
    """
    if not mobly_output_path or not testbed_name:
        raise ApiException('Arguments must be non-empty.')
    return os.path.join(
        get_latest_test_output_dir_symlink_path(
            mobly_output_path, testbed_name), records.OUTPUT_FILE_SUMMARY)


# TODO(fxbug.dev/119213) - Update |controllers| type to use HoneyDew's
# definition. When HoneyDew's Mobly device class is available, we
# should use that class as the Pytype to reduce the chance of controller
# instantiation error.
def new_testbed_config(
        testbed_name: str, log_path: str, mobly_controllers: List[Dict[str,
                                                                       Any]],
        test_params_dict: MoblyConfigComponent,
        botanist_honeydew_map: Dict[str, str]) -> MoblyConfigComponent:
    """Returns a Mobly testbed config which is required for running Mobly tests.

    This method expects the |controller| object to follow the schema of
    tools/botanist/cmd/run.go's |targetInfo| struct.

    Example |mobly_controllers|:
       [{
          "type": "FuchsiaDevice",
          "nodename":"fuchsia-54b2-030e-eb19",
          "ipv4":"192.168.42.112",
          "ipv6":"",
          "serial_socket":"/tmp/fuchsia-54b2-030e-eb19_mux",
          "ssh_key":"/etc/botanist/keys/pkey_infra"
       }, {
          "type": "AccessPoint",
          "ip": "192.168.42.11",
       }]

    Example output:
       {
          "TestBeds": [
            {
              "Name": "LocalTestbed",
              "Controllers": {
                "FuchsiaDevice": [
                  {
                    "name":"fuchsia-54b2-030e-eb19",
                    "ipv4":"192.168.42.112",
                    "ipv6":"",
                    "serial_socket":"/tmp/fuchsia-54b2-030e-eb19_mux",
                    "ssh_private_key":"/etc/botanist/keys/pkey_infra"
                  }
                ],
                "AccessPoint": [
                  {
                    "ip": "192.168.42.11"
                  }
                ]
              },
              "TestParams": {
                "test_dir": "/tmp/out"
              }
            }
          ]
        }

    Args:
        testbed_name: Mobly testbed name to use.
        log_path: absolute path to Mobly's top-level output directory.
        mobly_controllers: List of Mobly controller objects.
        test_params_dict: Mobly testbed params dictionary.
        botanist_honeydew_map: Dictionary that maps Botanist config names to
                               Honeydew config names.
    Returns:
      A Mobly Config that corresponds to the user-specified arguments.
    """
    controllers = {}
    for controller in mobly_controllers:
        controller_type = controller['type']
        del controller['type']
        # Convert botanist key names to relative Honeydew key names for fuchsia
        # devices. This is done here so that Honeydew does not have to do
        # the conversions itself.
        if api_infra.FUCHSIA_DEVICE == controller_type:
            for botanist_key, honeydew_key in botanist_honeydew_map.items():
                if botanist_key in controller:
                    controller[honeydew_key] = controller.pop(botanist_key)
        if controller_type in controllers:
            controllers[controller_type].append(controller)
        else:
            controllers[controller_type] = [controller]

    config_dict = {
        keys.Config.key_testbed.value:
            [
                {
                    keys.Config.key_testbed_name.value: testbed_name,
                    keys.Config.key_testbed_controllers.value: controllers,
                },
            ],
        keys.Config.key_mobly_params.value:
            {
                keys.Config.key_log_path.value: log_path
            }
    }

    return get_config_with_test_params(config_dict, test_params_dict)


def get_config_with_test_params(
        config_dict: MoblyConfigComponent,
        params_dict: MoblyConfigComponent) -> MoblyConfigComponent:
    """Returns a Mobly config with a populated 'TestParams' field.

    Replaces the field if it already exists.

    Args:
        config_dict: The Mobly config dictionary to update.
        params_dict: The Mobly testbed params dictionary to add to the config.

    Returns:
      A MoblyConfigComponent object.

    Raises:
      ApiException if |config_dict| is invalid.
    """
    try:
        ret = config_dict.copy()
        for tb in ret[keys.Config.key_testbed.value]:
            tb[keys.Config.key_testbed_test_params.value] = params_dict
        return ret
    except (AttributeError, KeyError, TypeError) as e:
        raise ApiException('Unexpected Mobly config content: %s' % e)


def set_transport_in_config(mobly_config: MoblyConfigComponent, transport: str):
    """Updates a mobly config to ensure the fuchsia devices in the config use
    the specified transport.

    Args:
      mobly_config: Mobly config object to update.
      transport: Transport to set on fuchsia devices in the Mobly config.
    """
    for testbed in mobly_config.get(keys.Config.key_testbed.value, []):
        controllers = testbed.get(keys.Config.key_testbed_controllers.value, {})
        for device in controllers.get(MOBLY_CONTROLLER_FUCHSIA_DEVICE, []):
            device[TRANSPORT_KEY] = transport
