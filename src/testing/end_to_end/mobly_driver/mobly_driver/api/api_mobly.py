#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Contains all Mobly APIs used in Mobly Driver."""

import os
from typing import Any

from mobly import keys, records

from . import api_infra


LATEST_RES_SYMLINK_NAME: str = "latest"

# Fuchsia-specific keys and values used in Mobly configs.
# Defined and used in
# https://osscs.corp.google.com/fuchsia/fuchsia/+/main:src/testing/end_to_end/mobly_controller/fuchsia_device.py
MOBLY_CONTROLLER_FUCHSIA_DEVICE: str = "FuchsiaDevice"
TRANSPORT_KEY: str = "transport"
FFX_PATH_KEY: str = "ffx_path"
FFX_SUBTOOLS_SEARCH_PATH_KEY: str = "ffx_subtools_search_path"

MoblyConfigComponent = dict[str, Any]


class ApiException(Exception):
    pass


def get_latest_test_output_dir_symlink_path(
    mobly_output_path: str, testbed_name: str
) -> str:
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
        raise ApiException("Arguments must be non-empty.")
    return os.path.join(
        mobly_output_path, testbed_name, LATEST_RES_SYMLINK_NAME
    )


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
        raise ApiException("Arguments must be non-empty.")
    return os.path.join(
        get_latest_test_output_dir_symlink_path(
            mobly_output_path, testbed_name
        ),
        records.OUTPUT_FILE_SUMMARY,
    )


# TODO(https://fxbug.dev/42070262) - Update |controllers| type to use Honeydew's
# definition. When Honeydew's Mobly device class is available, we
# should use that class as the Pytype to reduce the chance of controller
# instantiation error.
def new_testbed_config(
    testbed_name: str,
    output_path: str,
    ffx_path: str,
    transport: str,
    mobly_controllers: list[dict[str, Any]],
    test_params_dict: MoblyConfigComponent,
    botanist_honeydew_map: dict[str, str],
    ffx_subtools_search_path: str | None,
) -> MoblyConfigComponent:
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
                    "ssh_private_key":"/etc/botanist/keys/pkey_infra",
                    "ffx_path":"/path/to/ffx",
                    "transport":"fuchsia-controller",
                    "ffx_subtools_search_path":"/path/to/ffx/subtools"
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
        output_path: absolute path to Mobly's top-level output directory.
        ffx_path: absolute path to the FFX binary.
        transport: host->device transport type to use.
        mobly_controllers: List of Mobly controller objects.
        test_params_dict: Mobly testbed params dictionary.
        botanist_honeydew_map: Dictionary that maps Botanist config names to
                               Honeydew config names.
        ffx_subtools_search_path: absolute path to where to search for FFX plugins.
    Returns:
      A Mobly Config that corresponds to the user-specified arguments.
    """
    controllers: dict[str, list[dict[str, Any]]] = {}
    for controller in mobly_controllers:
        controller_type = controller["type"]
        del controller["type"]
        if api_infra.FUCHSIA_DEVICE == controller_type:
            # Add the "ffx_path" field for every Fuchsia device.
            controller[FFX_PATH_KEY] = ffx_path
            # Add the "transport" field for every Fuchsia device.
            controller[TRANSPORT_KEY] = transport
            # Convert botanist key names to relative Honeydew key names for
            # fuchsia devices. This is done here so that Honeydew does not have
            # to do the conversions itself.
            for botanist_key, honeydew_key in botanist_honeydew_map.items():
                if botanist_key in controller:
                    controller[honeydew_key] = controller.pop(botanist_key)
            # Add ffx subtools search path.
            if ffx_subtools_search_path:
                controller[
                    FFX_SUBTOOLS_SEARCH_PATH_KEY
                ] = ffx_subtools_search_path
        if controller_type in controllers:
            controllers[controller_type].append(controller)
        else:
            controllers[controller_type] = [controller]

    config_dict = {
        keys.Config.key_testbed.value: [
            {
                keys.Config.key_testbed_name.value: testbed_name,
                keys.Config.key_testbed_controllers.value: controllers,
            },
        ],
        keys.Config.key_mobly_params.value: {
            keys.Config.key_log_path.value: output_path
        },
    }

    return get_config_with_test_params(config_dict, test_params_dict)


def get_config_with_test_params(
    config_dict: MoblyConfigComponent, params_dict: MoblyConfigComponent
) -> MoblyConfigComponent:
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
        raise ApiException("Unexpected Mobly config content: %s" % e)


def set_transport(mobly_config: MoblyConfigComponent, transport: str) -> None:
    """Updates all fuchsia device configs to use the specified transport.

    Overwrites the existing value if the key already exists.

    Args:
      mobly_config: Mobly config object to update.
      transport: Transport to set on fuchsia devices in the Mobly config.
    """
    _set_per_device_config(mobly_config, TRANSPORT_KEY, transport)


def set_ffx_path(mobly_config: MoblyConfigComponent, ffx_path: str) -> None:
    """Updates all fuchsia device configs to use the specified ffx_path.

    Overwrites the existing value if the key already exists.

    Args:
      mobly_config: Mobly config object to update.
      ffx_path: FFX path to set on fuchsia devices in the Mobly config.
    """
    _set_per_device_config(mobly_config, FFX_PATH_KEY, ffx_path)


def set_ffx_subtools_search_path(
    mobly_config: MoblyConfigComponent, subtools_search_path: str
) -> None:
    """Updates all fuchsia device configs to use the specified ffx_path.

    Overwrites the existing value if the key already exists.

    Args:
      mobly_config: Mobly config object to update.
      subtools_search_path: absolute path to where to search for FFX plugins..
    """
    _set_per_device_config(
        mobly_config, FFX_SUBTOOLS_SEARCH_PATH_KEY, subtools_search_path
    )


def _set_per_device_config(
    mobly_config: MoblyConfigComponent, key: str, value: object
) -> None:
    """Updates all fuchsia device configs to contain a key-value pair.

    Overwrites the existing value if the key already exists.

    Args:
      mobly_config: Mobly config object to update.
      key: Device config key to update.
      value: Config value to use.
    """
    for testbed in mobly_config.get(keys.Config.key_testbed.value, []):
        controllers = testbed.get(keys.Config.key_testbed_controllers.value, {})
        for device in controllers.get(MOBLY_CONTROLLER_FUCHSIA_DEVICE, []):
            device[key] = value
