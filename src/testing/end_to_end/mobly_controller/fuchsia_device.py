#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly Controller for Fuchsia Device"""

import logging
from typing import Any, Dict, List

import honeydew
from honeydew import custom_types
from honeydew import transports
from honeydew.interfaces.device_classes import \
    fuchsia_device as fuchsia_device_interface
from honeydew.utils import properties

MOBLY_CONTROLLER_CONFIG_NAME = "FuchsiaDevice"

_LOGGER: logging.Logger = logging.getLogger(__name__)


def create(
    configs: List[Dict[str,
                       Any]]) -> List[fuchsia_device_interface.FuchsiaDevice]:
    """Create Fuchsia device controller(s) and returns them.

    Required for Mobly controller registration.

    Args:
        configs: List of dicts. Each dict representing a configuration for a
            Fuchsia device.

            Ensure to have following keys in the config dict:
            * name - Device name returned by `ffx target list`.
            * ssh_key - Absolute path to the SSH private key file needed to SSH
                into fuchsia device.
            * ssh_user - Username to be used to SSH into fuchsia device.
            * transport - Transport to be used to perform the host-target
                interactions.

    Returns:
        A list of FuchsiaDevice objects.
    """
    _LOGGER.debug(
        "FuchsiaDevice controller configs received in testbed yml file is '%s'",
        configs)

    fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice] = []
    for config in configs:
        device_config: Dict[str, Any] = _get_device_config(config)
        fuchsia_devices.append(
            honeydew.create_device(
                device_name=device_config["name"],
                ssh_private_key=device_config.get("ssh_private_key"),
                ssh_user=device_config.get("ssh_user"),
                transport=device_config.get("transport"),
                device_ip_port=device_config.get("device_ip_port")))
    return fuchsia_devices


def destroy(
        fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice]) -> None:
    """Closes all created fuchsia devices.

    Required for Mobly controller registration.

    Args:
        fuchsia_devices: A list of FuchsiaDevice objects.
    """
    for fuchsia_device in fuchsia_devices:
        fuchsia_device.close()


def get_info(
    fuchsia_devices: List[fuchsia_device_interface.FuchsiaDevice]
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
        fuchsia_device: fuchsia_device_interface.FuchsiaDevice
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
            attr_type: Any | None = getattr(type(fuchsia_device), attr, None)
            if isinstance(attr_type, properties.DynamicProperty):
                device_info["dynamic"][attr] = getattr(fuchsia_device, attr)
            elif isinstance(attr_type, properties.PersistentProperty):
                device_info["persistent"][attr] = getattr(fuchsia_device, attr)
        except NotImplementedError:
            pass

    return device_info


# TODO(fxbug.dev/123746): Remove this after fxbug.dev/123746 is fixed.
def _get_device_config(config: Dict[str, str]) -> Dict[str, Any]:
    """Parse, validate and update the mobly configuration associated with
    FuchsiaDevice controller.

    Args:
        config: mobly configuration associated with FuchsiaDevice controller.

    Returns:
        Validated mobly configuration associated with FuchsiaDevice controller.

    Raises:
        RuntimeError: If any required information is missing.
        ValueError:   If the transport is invalid, or the device_ip_port is invalid
    """
    _LOGGER.debug(
        "FuchsiaDevice controller config received in testbed yml file is '%s'",
        config)

    # Sample testbed file format for FuchsiaDevice controller used in infra...
    # - Controllers:
    #     FuchsiaDevice:
    #     - ipv4: ''
    #       ipv6: fe80::93e3:e3d4:b314:6e9b%qemu
    #       nodename: botanist-target-qemu
    #       serial_socket: ''
    #       ssh_key: private_key
    #       device_ip_port: IpPort
    device_config: Dict[str, Any] = {
        "ssh_user": config.get("ssh_user"),
    }

    if config.get("name"):
        device_config["name"] = config["name"]
    elif config.get("nodename"):
        device_config["name"] = config["nodename"]
    else:
        raise RuntimeError("Missing fuchsia device name in the config")

    if config.get("ssh_private_key"):
        device_config["ssh_private_key"] = config["ssh_private_key"]
    elif config.get("nodename"):
        device_config["ssh_private_key"] = config["ssh_key"]

    if config.get("device_ip_port"):
        try:
            device_config["device_ip_port"] = custom_types.IpPort.parse(
                config["device_ip_port"])
        except Exception as err:  # pylint: disable=broad-except
            raise ValueError(
                f"Invalid value for 'device_ip_port': {config['device_ip_port']} "
            ) from err

    if config.get("transport"):
        transport: str = config["transport"]

        if transport == "sl4f":
            device_config["transport"] = transports.TRANSPORT.SL4F
        elif transport in transports.FUCHSIA_CONTROLLER_TRANSPORTS:
            device_config["transport"] = transports.TRANSPORT.FUCHSIA_CONTROLLER
        else:
            raise ValueError(
                f"Invalid transport '{transport}' passed for " \
                f"{device_config['name']}"
            )

    _LOGGER.debug(
        "Updated FuchsiaDevice controller config after the validation is '%s'",
        config)

    return device_config
