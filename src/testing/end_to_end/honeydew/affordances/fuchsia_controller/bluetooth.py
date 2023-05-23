#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Bluetooth affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances import bluetooth


class Bluetooth(bluetooth.Bluetooth):
    """Bluetooth affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def sys_init(self) -> None:
        """Initializes bluetooth stack.

        Raises:
            errors.FuchsiaDeviceError: On failure.
        """
        raise NotImplementedError

    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.
        """
        raise NotImplementedError
