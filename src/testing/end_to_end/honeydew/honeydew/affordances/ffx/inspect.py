# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Inspect affordance implementation using the FFX."""

import json
import logging
from typing import Any

import fuchsia_inspect

from honeydew import errors
from honeydew.interfaces.affordances import inspect
from honeydew.transports import ffx as ffx_transport

_LOGGER: logging.Logger = logging.getLogger(__name__)


class Inspect(inspect.Inspect):
    """Inspect affordance implementation using the FFX.

    Args:
        device_name: Device name returned by `ffx target list`.
        ffx: ffx_transport.FFX.
    """

    def __init__(self, device_name: str, ffx: ffx_transport.FFX) -> None:
        self._name: str = device_name
        self._ffx: ffx_transport.FFX = ffx

    def get_data(
        self,
        selectors: list[str] | None = None,
        monikers: list[str] | None = None,
    ) -> fuchsia_inspect.InspectDataCollection:
        """Return the inspect data associated with the given selectors and
        monikers.

        Args:
            selectors: selectors to be queried.
            monikers: component monikers.

        Note: If both `selectors` and `monikers` lists are empty, inspect data
        for the whole system will be returned.

        Returns:
            Inspect data collection

        Raises:
            InspectError: Failed to return inspect data.
        """
        selectors_and_monikers: list[str] = []
        if selectors:
            selectors_and_monikers += selectors
        if monikers:
            for moniker in monikers:
                selectors_and_monikers.append(moniker.replace(":", r"\:"))

        cmd: list[str] = [
            "--machine",
            "json",
            "inspect",
            "show",
        ] + selectors_and_monikers

        try:
            message: str = f"Collecting the inspect data from {self._name}"
            if selectors:
                message = f"{message}, with selectors={selectors}"
            if monikers:
                message = f"{message}, with monikers={monikers}"
            _LOGGER.info(message)
            inspect_data_json_str: str = self._ffx.run(
                cmd=cmd,
                log_output=False,
            )
            _LOGGER.info("Collected the inspect data from %s.", self._name)

            inspect_data_json_obj: list[dict[str, Any]] = json.loads(
                inspect_data_json_str
            )
            return fuchsia_inspect.InspectDataCollection.from_list(
                inspect_data_json_obj
            )
        except (
            errors.FfxCommandError,
            errors.DeviceNotConnectedError,
            fuchsia_inspect.InspectDataError,
        ) as err:
            raise errors.InspectError(
                f"Failed to collect the inspect data from {self._name}"
            ) from err
