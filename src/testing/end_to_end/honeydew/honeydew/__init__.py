#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""HoneyDew python module."""

import logging
import subprocess
from typing import Type

from honeydew import custom_types
from honeydew import errors
from honeydew import transports
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.fuchsia_device.sl4f import fuchsia_device as sl4f_fuchsia_device
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import ffx as ffx_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)

_AFFORDANCE_NOT_IMPLEMENTED: str = "raise NotImplementedError"


# List all the public methods in alphabetical order
def create_device(
    device_name: str,
    ssh_private_key: str | None = None,
    ssh_user: str | None = None,
    transport: transports.TRANSPORT | None = None,
    device_ip_port: custom_types.IpPort | None = None,
) -> fuchsia_device_interface.FuchsiaDevice:
    """Factory method that creates and returns the device class.

    This method will look at all the device class implementations available, and
    if it finds a match it will return the corresponding device class object.
    If not, GenericFuchsiaDevice instance will be returned.

    Args:
        device_name: Device name returned by `ffx target list`.

        ssh_private_key: Absolute path to the SSH private key file needed to SSH
            into fuchsia device.

        ssh_user: Username to be used to SSH into fuchsia device.
            Default is "fuchsia".

        transport: Transport to use to perform host-target interactions.
            If not set, transports.DEFAULT_TRANSPORT will be used.

        device_ip_port: Ip Address and port of the target to create.
            If specified, this will cause the device to be added and tracked
            by ffx.

    Returns:
        Fuchsia device object

    Raises:
        errors.FuchsiaDeviceError: Failed to create Fuchsia device object.
        errors.FfxCommandError: Failure in running an FFX Command.
    """
    if transport is None:
        transport = transports.DEFAULT_TRANSPORT

    if device_ip_port:
        _add_and_verify_device(device_name, device_ip_port)

    try:
        device_class: Type[
            fuchsia_device_interface.FuchsiaDevice
        ] = _get_device_class(transport)

        return device_class(
            device_name, ssh_private_key, ssh_user
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
):
    """Adds the device to the ffx target collection and verifies names match.

    If the device is already in the collection, only verifies names match.

    Args:
        device_name: Device name returned by `ffx target list`.

        device_ip: Ip Address of the target to create. If specified, this will
            cause the device to be added and tracked by ffx.

    Raises:
        errors.FfxCommandError: Failed to add device.
    """
    try:
        if not _target_exists(device_ip_port):
            _LOGGER.debug("Adding target '%s'", device_ip_port)
            ffx_transport.FFX.add_target(device_ip_port)
        ffx: ffx_transport.FFX = ffx_transport.FFX(target=str(device_ip_port))
        reported_device_name = ffx.get_target_name()
        if reported_device_name != device_name:
            raise ValueError(
                f"Target name reported for IpPort {device_ip_port}, "
                f"{reported_device_name}, did not match provided "
                f"device_name {device_name}"
            )
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FfxCommandError(f"Failed to add {device_name}") from err


def _target_exists(device_ip_port: custom_types.IpPort) -> bool:
    try:
        ffx: ffx_transport.FFX = ffx_transport.FFX(target=str(device_ip_port))

        ffx.get_target_information()
        return True
    except subprocess.TimeoutExpired:
        # If this raises a timeout exception, the target is unreachable and
        # therefore doesn't exist.
        return False
    except Exception as err:  # pylint: disable=broad-except
        raise errors.FuchsiaDeviceError(
            f"Failure determining if Target exists at: {device_ip_port}"
        ) from err
