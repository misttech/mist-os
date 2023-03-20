#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for a bluetooth capable device."""

import abc

from honeydew.interfaces.affordances import bluetooth
from honeydew.utils import properties


# pylint: disable=too-few-public-methods
class BluetoothCapableDevice(abc.ABC):
    """Abstract base class to be implemented by a device which supports the
    Bluetooth affordance."""

    @properties.Affordance
    @abc.abstractmethod
    def bluetooth(self) -> bluetooth.Bluetooth:
        """Returns a bluetooth affordance object.

        Returns:
            bluetooth.Bluetooth object
        """
