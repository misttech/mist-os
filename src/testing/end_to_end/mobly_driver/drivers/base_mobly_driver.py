#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Defines the abstract class for a Mobly test driver.

See mobly_driver_lib.py's `run()` method to understand how BaseDriver's
interface is used.
"""
from abc import ABC, abstractmethod
from typing import Optional


class BaseDriver(ABC):
    """Abstract base class for a Mobly test driver.
    This class contains abstract methods that are meant to be overridden to
    provide environment-specific implementations.
    """

    def __init__(self, params_path: Optional[str] = None) -> None:
        """Initializes the instance.
        Args:
          params_path: absolute path to the Mobly testbed params file.
        """
        self._params_path = params_path

    @abstractmethod
    def generate_test_config(self) -> str:
        """Returns a Mobly test config in YAML format.
        The Mobly test config is a required input file of any Mobly tests.
        It includes information on the DUT(s) and specifies test parameters.
        Example output:
        ---
        TestBeds:
        - Name: SomeName
          Controllers:
            FuchsiaDevice:
            - name: fuchsia-1234-5678-90ab
          TestParams:
            param_1: "val_1"
            param_2: "val_2"
        Returns:
          A YAML string that represents a Mobly test config.
        """
        pass

    @abstractmethod
    def teardown(self) -> None:
        """Performs any required clean up upon Mobly test completion."""
        pass
