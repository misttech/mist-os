#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Honeydew python module."""

import logging

from honeydew import errors
from honeydew.fuchsia_device.fuchsia_controller import (
    fuchsia_device as fc_fuchsia_device,
)
from honeydew.fuchsia_device.fuchsia_controller_preferred import (
    fuchsia_device as fc_preferred_fuchsia_device,
)
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.transports import ffx
from honeydew.typing import custom_types

_LOGGER: logging.Logger = logging.getLogger(__name__)


# List all the public methods
def create_device(
    device_name: str,
    transport: custom_types.TRANSPORT,
    ffx_config: custom_types.FFXConfig,
    device_ip_port: custom_types.IpPort | None = None,
    ssh_private_key: str | None = None,
    ssh_user: str | None = None,
) -> fuchsia_device_interface.FuchsiaDevice:
    """Factory method that creates and returns the device class.

    Args:
        device_name: Device name returned by `ffx target list`.

        transport: Transport to use to perform host-target interactions.

        ffx_config: Ffx configuration that need to be used while running ffx
            commands.

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
    try:
        if device_ip_port:
            _LOGGER.info(
                "CAUTION: device_ip_port='%s' argument has been passed. Please "
                "make sure this value associated with the device is persistent "
                "across the reboots. Otherwise, host-target interactions will not "
                "work consistently.",
                device_ip_port,
            )

            ffx_transport: ffx.FFX = ffx.FFX(
                target_name=device_name,
                target_ip_port=device_ip_port,
                config=ffx_config,
            )
            ffx_transport.add_target()

        if transport == custom_types.TRANSPORT.FUCHSIA_CONTROLLER:
            return fc_fuchsia_device.FuchsiaDevice(
                device_name,
                ffx_config,
                device_ip_port,
                ssh_private_key,
                ssh_user,
            )
        else:  # transport == custom_types.TRANSPORT.FUCHSIA_CONTROLLER_PREFERRED:
            return fc_preferred_fuchsia_device.FuchsiaDevice(
                device_name,
                ffx_config,
                device_ip_port,
                ssh_private_key,
                ssh_user,
            )
    except Exception as err:
        raise errors.FuchsiaDeviceError(
            f"Failed to create device for '{device_name}': {err}"
        ) from err
