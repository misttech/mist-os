#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for Bluetooth affordance."""

import abc


class BluetoothGap(abc.ABC):
    """Abstract base class for BluetoothGap affordance."""

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def sys_init(self) -> None:
        """Initializes bluetooth stack."""

    @abc.abstractmethod
    def request_discovery(self, discovery: bool) -> None:
        """Requests Bluetooth Discovery on Bluetooth capable device.

        Args:
            discovery: True to start discovery, False to stop discovery.
        """

    @abc.abstractmethod
    def set_discoverable(self, discoverable: bool) -> None:
        """Sets device to be discoverable by others.

        Args:
            discoverable: True to be discoverable by others, False to be not
                          discoverable by others.
        """
