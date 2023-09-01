# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly Controller for Fuchsia Device (controlled via Fuchsia Controller)."""

from typing import Any
from typing import Dict
from typing import List

from fuchsia_controller_py import Context
from fuchsia_controller_py import IsolateDir

MOBLY_CONTROLLER_CONFIG_NAME = "FuchsiaDevice"


class FuchsiaDevice(object):

    def __init__(self, ctx: Context, config: Dict[str, Any]):
        self.config = config
        self.ctx = ctx


def create(configs: List[Dict[str, Any]]) -> List[FuchsiaDevice]:
    """Creates the all Fuchsia devices for tests.

    Args:
        configs: The list of configs describing each Fuchsia device.

    Returns:
        List of Fuchsia devices. Each instance is isolated, containing a config (for reference),
        and a fuchsia controller `Context`.

    Raises:
        ValueError: in the event that a config value lacks an "ipv4," "ipv6," or "name" key.
    """
    res = []
    for config in configs:
        isolate = IsolateDir()
        ctx_config = {
            "sdk.root": ".",
        }
        target = config.get("ipv6") or config.get("ipv4") or config.get("name")
        if not target:
            raise ValueError(
                f"Unable to load config properly. Target has no address or name: {config}"
            )
        ctx = Context(isolate_dir=isolate, target=target, config=ctx_config)
        res.append(FuchsiaDevice(ctx, config))
    return res


def destroy(_: List[FuchsiaDevice]) -> None:
    """Destroys all listed Fuchsia devices.

    Args:
        _: The list of Fuchsia devices being destroyed.
    """
    pass


def get_info(fuchsia_devices: List[FuchsiaDevice]) -> List[Dict[str, Any]]:
    """Returns all info of each Fuchsia device.

    Args:
        fuchsia_devices: The list of Fuchsia devices being queried.

    Returns:
        The config for each Fuchsia device.
    """
    res = []
    for device in fuchsia_devices:
        res.append(device.config)
    return res
