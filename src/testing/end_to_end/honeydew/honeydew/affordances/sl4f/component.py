#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Component affordance implementation using SL4F."""

from typing import Any, Dict, List

from honeydew.interfaces.affordances import component
from honeydew.transports import sl4f as sl4f_transport

_SL4F_METHODS: Dict[str, str] = {
    "Launch": "component_facade.Launch",
    "List": "component_facade.List",
    "Search": "component_facade.Search",
}


class Component(component.Component):
    """Component affordance implementation using SL4F.

    Args:
        device_name: Device name returned by `ffx target list`.
        sl4f: SL4F transport.
    """

    def __init__(self, device_name: str, sl4f: sl4f_transport.SL4F) -> None:
        self._name: str = device_name
        self._sl4f: sl4f_transport.SL4F = sl4f

    # List all the public methods in alphabetical order
    def launch(self, url: str) -> None:
        """Launches a (V2) component.

        Args:
            url: url of the component ending in ".cm"

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        self._sl4f.run(method=_SL4F_METHODS["Launch"], params={"url": url})

    def list(self) -> List[str]:
        """Returns the list of components that are currently running.

        Returns:
            List containing full component URLs that are currently running.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        list_components_resp: Dict[str, Any] = self._sl4f.run(
            method=_SL4F_METHODS["List"])
        return list_components_resp.get("result", [])

    def search(self, name: str) -> bool:
        """Searches for a component under returns True if found.

        Args:
            name: name of the component ending in ".cm"

        Returns:
            True if component is found else False.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        search_component_resp: Dict[str, Any] = self._sl4f.run(
            method=_SL4F_METHODS["Search"], params={"name": name})

        return search_component_resp.get("result", "") == "Success"
