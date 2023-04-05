#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Abstract base class for power switch hardware."""

from __future__ import annotations

import abc
from typing import Dict, Optional, Union


class PowerSwitch(abc.ABC):
    """Abstract base class for power switch hardware."""

    @classmethod
    @abc.abstractmethod
    def from_station_config(
            cls, config: Dict[str, Union[str, float, int]]) -> PowerSwitch:
        """Class method to instantiate the power switch class.

        Args:
            config: Power switch configuration dict.

        Returns:
            PowerSwitch: PowerSwitch object.
        """

    # List all the public methods in alphabetical order
    @abc.abstractmethod
    def power_off(self, outlet: Optional[int] = None) -> None:
        """Turns off the power at the specified outlet on the power switch.

        Args:
            outlet: outlet on which we need to perform this operation.
        """

    @abc.abstractmethod
    def power_on(self, outlet: Optional[int] = None) -> None:
        """Turns on the power at the specified outlet on the power switch.

        Args:
            outlet: outlet on which we need to perform this operation.
        """
