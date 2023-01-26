#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all Mobly APIs used in Mobly Driver."""

import os
from mobly import keys
from mobly import records
from typing import List, Dict, Any

LATEST_RES_SYMLINK_NAME = 'latest'

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


# TODO(fxbug.dev/119213) - Update |fuchsia_controllers| type to use HoneyDew's
# definition. When HoneyDew's FuchsiaDevice Mobly device class is available, we
# should use that class as the Pytype to reduce the chance of controller
# instantiation error.
def new_testbed_config(
        testbed_name: str, log_path: str, fuchsia_controllers: List[Dict[str,
                                                                         Any]],
        test_params_dict: MoblyConfigComponent) -> MoblyConfigComponent:
    """Returns a Mobly testbed config which is required for running Mobly tests.

    This method expects the |fuchsia_controller| object to follow the schema of
    tools/botanist/cmd/run.go's |targetInfo| struct.

    Example object from |fuchsia_controllers|:
       {
          "nodename":"fuchsia-54b2-030e-eb19",
          "ipv4":"192.168.42.112",
          "ipv6":"",
          "serial_socket":"/tmp/fuchsia-54b2-030e-eb19_mux",
          "ssh_key":"/etc/botanist/keys/pkey_infra"
       }

    Example output:
       {
          "TestBeds": [
            {
              "Name": "LocalTestbed",
              "Controllers": {
                "FuchsiaDevice": [
                  {
                    "nodename":"fuchsia-54b2-030e-eb19",
                    "ipv4":"192.168.42.112",
                    "ipv6":"",
                    "serial_socket":"/tmp/fuchsia-54b2-030e-eb19_mux",
                    "ssh_key":"/etc/botanist/keys/pkey_infra"
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
        fuchsia_controllers: List of FuchsiaDevice Mobly controller objects.
        test_params_dict: Mobly testbed params dictionary.

    Returns:
      A Mobly Config that corresponds to the user-specified arguments.
    """
    config_dict = {
        keys.Config.key_testbed.value:
            [
                {
                    keys.Config.key_testbed_name.value: testbed_name,
                    keys.Config.key_testbed_controllers.value:
                        {
                            # TODO(fxbug.dev/119213) - Replace using HoneyDew.
                            'FuchsiaDevice': fuchsia_controllers
                        },
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
