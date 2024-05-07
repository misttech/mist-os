#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly Controller for Fuchsia Device"""

import logging
from typing import Any, Dict, List

import honeydew
from honeydew.typing import custom_types
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import ffx
from honeydew.utils import properties

MOBLY_CONTROLLER_CONFIG_NAME = "FuchsiaDevice"

_LOGGER: logging.Logger = logging.getLogger(__name__)

_FFX_CONFIG_OBJ: ffx.FfxConfig = ffx.FfxConfig()


def create(
    configs: List[Dict[str, Any]]
) -> List[fuchsia_device_interface.FuchsiaDevice]:
    """Create Fuchsia device controller(s) and returns them.

    Required for Mobly controller registration.

    Args:
        configs: List of dicts. Each dict representing a configuration for a
            Fuchsia device.

            Ensure to have following keys in the config dict:
            * name - Device name returned by `ffx target list`.
            * ssh_private_key - Absolute path to the SSH private key file needed
                to SSH into fuchsia device.
            * ssh_user - Username to be used to SSH into fuchsia device.
            * transport - Transport to be used to perform the host-target
                interactions.
            * ffx_path - Absolute path to FFX binary to use for the device.
    Returns:
        A list of FuchsiaDevice objects.
    """
    _LOGGER.debug(
        "FuchsiaDevice controller configs received in testbed yml file is '%s'",
        configs,
    )

    test_logs_dir: str = _get_log_directory()
    ffx_path: str = _get_ffx_path(configs)

    # Call `FfxConfig.setup` before calling `create_device` as
    # `create_device` results in calling an FFX command and we
    # don't want to miss those FFX logs

    # Note - As of now same FFX Config is used across all fuchsia devices.
    # This means we will have one FFX daemon running which will talk to all
    # fuchsia devices in the testbed.
    # This is okay for in-tree use cases but may not work for OOT cases where
    # each fuchsia device may be running different build that require different
    # FFX version.
    # This will also not work if you have 2 devices in testbed with one uses
    # device_ip and one uses device_name for FFX commands. This should not
    # happen in our setups though as we will either have mdns enabled or
    # disabled on host.
    # Right fix for all such cases is to use separate ffx config per device.
    # This support will be added when needed in future.
    _FFX_CONFIG_OBJ.setup(
        binary_path=ffx_path,
        isolate_dir=None,
        logs_dir=f"{test_logs_dir}/ffx/",
        logs_level=None,
        enable_mdns=_enable_mdns(configs),
        subtools_search_path=_get_ffx_subtools_search_path(configs),
    )

    fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice] = []
    for config in configs:
        device_config: Dict[str, Any] = _parse_device_config(config)
        fuchsia_devices.append(
            honeydew.create_device(
                device_name=device_config["name"],
                transport=device_config["transport"],
                ffx_config=_FFX_CONFIG_OBJ.get_config(),
                device_ip_port=device_config.get("device_ip_port"),
                ssh_private_key=device_config.get("ssh_private_key"),
                ssh_user=device_config.get("ssh_user"),
            )
        )
    return fuchsia_devices


def destroy(
    fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice],
) -> None:
    """Closes all created fuchsia devices.

    Required for Mobly controller registration.

    Args:
        fuchsia_devices: A list of FuchsiaDevice objects.
    """
    for fuchsia_device in fuchsia_devices:
        fuchsia_device.close()

    # Call `FfxConfig.close` manually even though it's already registered for
    # clean up in `FfxConfig.setup` in order to minimize chance of FFX daemon
    # leak in the event that SIGKILL/SIGTERM is received between `destroy` and
    # test program exit.
    _FFX_CONFIG_OBJ.close()


def get_info(
    fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice],
) -> List[Dict[str, Any]]:
    """Gets information from a list of FuchsiaDevice objects.

    Optional for Mobly controller registration.

    Args:
        fuchsia_devices: A list of FuchsiaDevice objects.

    Returns:
        A list of dict, each representing info for an FuchsiaDevice objects.
    """
    return [
        _get_fuchsia_device_info(fuchsia_device)
        for fuchsia_device in fuchsia_devices
    ]


def _get_fuchsia_device_info(
    fuchsia_device: fuchsia_device_interface.FuchsiaDevice,
) -> Dict[str, Any]:
    """Returns information of a specific fuchsia device object.

    Args:
        fuchsia_device: FuchsiaDevice object.

    Returns:
        Dict containing information of a fuchsia device.
    """
    device_info: Dict[str, Any] = {
        "device_class": fuchsia_device.__class__.__name__,
        "persistent": {},
        "dynamic": {},
    }

    for attr in dir(fuchsia_device):
        if attr.startswith("_"):
            continue

        try:
            attr_type: Any = getattr(type(fuchsia_device), attr, None)
            if isinstance(attr_type, properties.DynamicProperty):
                device_info["dynamic"][attr] = getattr(fuchsia_device, attr)
            elif isinstance(attr_type, properties.PersistentProperty):
                device_info["persistent"][attr] = getattr(fuchsia_device, attr)
        except NotImplementedError:
            pass

    return device_info


def _enable_mdns(configs: List[Dict[str, Any]]) -> bool:
    for config in configs:
        device_config: Dict[str, Any] = _parse_device_config(config)
        if not device_config.get("device_ip_port"):
            return True
    return False


def _parse_device_config(config: Dict[str, str]) -> Dict[str, Any]:
    """Validates and parses mobly configuration associated with FuchsiaDevice.

    Args:
        config: The mobly configuration associated with FuchsiaDevice.

    Returns:
        Validated and parsed mobly configuration associated with FuchsiaDevice.

    Raises:
        RuntimeError: If the fuchsia device name in the config is missing.
        ValueError: If either transport device_ip_port is invalid.
    """
    _LOGGER.debug(
        "FuchsiaDevice controller config received in testbed yml file is '%s'",
        config,
    )

    # Sample testbed file format for FuchsiaDevice controller used in infra...
    # - Controllers:
    #     FuchsiaDevice:
    #     - ipv4: ''
    #       ipv6: fe80::93e3:e3d4:b314:6e9b%qemu
    #       nodename: botanist-target-qemu
    #       serial_socket: ''
    #       ssh_key: private_key
    #       transport: fuchsia-controller
    #       device_ip_port: [::1]:8022
    if "name" not in config:
        raise RuntimeError("Missing fuchsia device name in the config")

    if "transport" not in config:
        raise RuntimeError("Missing transport field in the config")

    device_config: Dict[str, Any] = {}

    for config_key, config_value in config.items():
        if config_key == "transport":
            device_config["transport"] = custom_types.TRANSPORT(
                config["transport"]
            )
        elif config_key == "device_ip_port":
            try:
                device_config[
                    "device_ip_port"
                ] = custom_types.IpPort.create_using_ip_and_port(config_value)
            except Exception as err:  # pylint: disable=broad-except
                raise ValueError(
                    f"Invalid device_ip_port `{config_value}` passed for "
                    f"{config['name']}"
                ) from err
        elif config_key in ["ipv4", "ipv6"]:
            if config.get("ipv4"):
                device_config[
                    "device_ip_port"
                ] = custom_types.IpPort.create_using_ip(config["ipv4"])
            if config.get("ipv6"):
                device_config[
                    "device_ip_port"
                ] = custom_types.IpPort.create_using_ip(config["ipv6"])
        else:
            device_config[config_key] = config_value

    _LOGGER.debug(
        "Updated FuchsiaDevice controller config after the validation is '%s'",
        device_config,
    )

    return device_config


def _get_log_directory() -> str:
    """Returns the path to the directory where logs should be stored.

    Returns:
        Directory path.
    """
    # TODO(https://fxbug.dev/42078903): Read log path from config once this issue is fixed
    return getattr(
        logging,
        "log_path",  # Set by Mobly in base_test.BaseTestClass.run.
    )


def _get_ffx_path(configs: List[Dict[str, Any]]) -> str:
    """Returns the path to the FFX binary to use.

    Args:
      configs: List of dicts. Each dict representing a configuration for a
            Fuchsia device.

    Returns:
        Absolute path to FFX.
    """
    # FFX CLI is currently global and not localized to the individual devices so
    # just return the the first "ffx_path" encountered.
    for config in configs:
        if "ffx_path" in config:
            return config["ffx_path"]
    raise RuntimeError("No FFX path found in any device config")


def _get_ffx_subtools_search_path(configs: List[Dict[str, Any]]) -> str | None:
    """Returns the ffx subtools search path to use.

    Args:
      configs: List of dicts. Each dict representing a configuration for a
            Fuchsia device.

    Returns:
        Absolute path to the subtools search path, or None.
    """
    # FFX CLI is currently global and not localized to the individual devices so
    # just return the the first "ffx_path" encountered.
    for config in configs:
        if "ffx_subtools_search_path" in config:
            return config["ffx_subtools_search_path"]
    return None
