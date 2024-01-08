#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""HoneyDew python module."""

import ipaddress
import logging
import subprocess
from typing import Type

from honeydew import custom_types, errors, transports
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.fuchsia_device.sl4f import fuchsia_device as sl4f_fuchsia_device
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import ffx as ffx_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)


# List all the public methods in alphabetical order
def create_device(
    device_name: str,
    transport: transports.TRANSPORT,
    device_ip_port: custom_types.IpPort | None = None,
    ssh_private_key: str | None = None,
    ssh_user: str | None = None,
) -> fuchsia_device_interface.FuchsiaDevice:
    """Factory method that creates and returns the device class.

    Args:
        device_name: Device name returned by `ffx target list`.

        transport: Transport to use to perform host-target interactions.

        device_ip_port: IP Address and port of the device.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

    Returns:
        Fuchsia device object

    Raises:
        errors.FuchsiaDeviceError: Failed to create Fuchsia device object.
        errors.FfxCommandError: Failure in running an FFX Command.
    """
    if device_ip_port:
        _LOGGER.info(
            "CAUTION: device_ip_port='%s' argument has been passed. Please "
            "make sure this value associated with the device is persistent "
            "across the reboots. Otherwise, host-target interactions will not "
            "work consistently.",
            device_ip_port,
        )

    if device_ip_port:
        _add_and_verify_device(device_name, device_ip_port)

    try:
        device_class: Type[
            fuchsia_device_interface.FuchsiaDevice
        ] = _get_device_class(transport)

        device_ip: ipaddress.IPv4Address | ipaddress.IPv6Address | None = None
        if device_ip_port and device_ip_port.ip:
            device_ip = device_ip_port.ip
        return device_class(
            device_name, device_ip, ssh_private_key, ssh_user
        )  # type: ignore[call-arg]
    except Exception as err:
        raise errors.FuchsiaDeviceError(
            f"Failed to create device for '{device_name}'"
        ) from err


# List all the private methods
def _get_device_class(
    transport: transports.TRANSPORT,
) -> Type[fuchsia_device_interface.FuchsiaDevice]:
    """Returns fuchsia_device for specified transport.

    Args:
        transport: Transport to use to perform host-target interactions.

    Returns:
        fuchsia_device_interface.FuchsiaDevice implementation using the
        specified transport.
    """
    device_class: Type[fuchsia_device_interface.FuchsiaDevice]

    if transport == transports.TRANSPORT.SL4F:
        device_class = sl4f_fuchsia_device.FuchsiaDevice
    else:  # transports.TRANSPORT.FUCHSIA_CONTROLLER
        device_class = fc_fuchsia_device.FuchsiaDevice

    return device_class


def _add_and_verify_device(
    device_name: str, device_ip_port: custom_types.IpPort
) -> None:
    """Adds the device to the ffx target collection and verifies names match.

    If the device is already in the collection, only verifies names match.

    Args:
        device_name: Device name returned by `ffx target list`.

        device_ip_port: Device IP and Port of the device.

    Raises:
        errors.FfxCommandError: Failed to add device.
    """
    try:
        if not _target_exists(device_name, device_ip_port):
            _LOGGER.debug("Adding target '%s'", device_ip_port)
            ffx_transport.FFX.add_target(device_ip_port)
        ffx: ffx_transport.FFX = ffx_transport.FFX(
            target_name=device_name, target_ip=device_ip_port.ip
        )
        reported_device_name: str = ffx.get_target_name()
        if reported_device_name != device_name:
            raise ValueError(
                f"Target name reported for IpPort {device_ip_port}, "
                f"{reported_device_name}, did not match provided "
                f"device_name {device_name}"
            )
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(f"Failed to add {device_name}") from err


def _target_exists(
    device_name: str, device_ip_port: custom_types.IpPort
) -> bool:
    """Returns true if ffx already discovers this target, false otherwise.

    Args:
        device_name: Device name returned by `ffx target list`.

        device_ip_port: Device IP and Port of the device.

    Raises:
        errors.FuchsiaDeviceError: Failed to determine if target exists or not.
    """
    try:
        ffx: ffx_transport.FFX = ffx_transport.FFX(
            target_name=device_name, target_ip=device_ip_port.ip
        )

        ffx.get_target_information()
        return True
    except (subprocess.TimeoutExpired, errors.DeviceNotConnectedError):
        # If this raises a timeout exception, the target is unreachable and
        # therefore doesn't exist. errors.DeviceNotConnectedError is also
        # surfaced from a timeout, so should be treated the same.
        return False
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FuchsiaDeviceError(
            f"Failure determining if Target exists at: {device_ip_port}"
        ) from err
