# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Honeydew python module."""

import logging
from typing import Any

from honeydew import errors
from honeydew.fuchsia_device import fuchsia_device
from honeydew.interfaces.device_classes import (
    fuchsia_device as fuchsia_device_interface,
)
from honeydew.typing import custom_types

_LOGGER: logging.Logger = logging.getLogger(__name__)


# List all the public methods
def create_device(
    device_info: custom_types.DeviceInfo,
    ffx_config: custom_types.FFXConfig,
    # intentionally made this a Dict instead of dataclass to minimize the changes in remaining Lacewing stack every time we need to add a new configuration item
    config: dict[str, Any] | None = None,
) -> fuchsia_device_interface.FuchsiaDevice:
    """Factory method that creates and returns the device class.

    Args:
        device_info: Fuchsia device information.

        ffx_config: Ffx configuration that need to be used while running ffx
            commands.

        config: Honeydew device configuration, if any.
            Format:
                {
                    "transports": {
                        <transport_name>: {
                            <key>: <value>,
                            ...
                        },
                        ...
                    },
                    "affordances": {
                        <affordance_name>: {
                            <key>: <value>,
                            ...
                        },
                        ...
                    },
                }
            Example:
                {
                    "transports": {
                        "fuchsia_controller": {
                            "timeout": 30,
                        }
                    },
                    "affordances": {
                        "bluetooth": {
                            "implementation": "fuchsia-controller",
                        },
                        "wlan": {
                            "implementation": "sl4f",
                        }
                    },
                }

    Returns:
        Fuchsia device object

    Raises:
        errors.FuchsiaDeviceError: Failed to create Fuchsia device object.
    """
    _LOGGER.debug("create_device has been called with: %s", locals())

    try:
        if device_info.ip_port:
            _LOGGER.info(
                "CAUTION: device_ip_port='%s' argument has been passed. Please "
                "make sure this value associated with the device is persistent "
                "across the reboots. Otherwise, host-target interactions will not "
                "work consistently.",
                device_info.ip_port,
            )

        return fuchsia_device.FuchsiaDevice(
            device_info,
            ffx_config,
            config,
        )
    except errors.HoneydewError as err:
        raise errors.FuchsiaDeviceError(
            f"Failed to create device for '{device_info.name}': {err}"
        ) from err
