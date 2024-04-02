#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Defines the abstract class for a Mobly test driver.

See mobly_driver_lib.py's `run()` method to understand how BaseDriver's
interface is used.
"""
import os
from abc import ABC, abstractmethod
from typing import Any, Optional


TEST_OUTDIR_ENV = "FUCHSIA_TEST_OUTDIR"


class BaseDriver(ABC):
    """Abstract base class for a Mobly test driver.
    This class contains abstract methods that are meant to be overridden to
    provide environment-specific implementations.
    """

    def __init__(
        self,
        ffx_path: str,
        transport: str,
        output_path: Optional[str] = None,
        params_path: Optional[str] = None,
        ffx_subtools_search_path: Optional[str] = None,
    ) -> None:
        """Initializes the instance.

        Args:
          ffx_path: absolute path to the FFX binary.
          transport: host->target transport type to use.
          output_path: absolute path to directory for storing Mobly test output.
          params_path: absolute path to the Mobly testbed params file.
          ffx_subtools_search_path: absolute path to where to search for FFX plugins.
        Raises:
          KeyError if required environment variables not found.
        """
        self._ffx_path = ffx_path
        self._transport = transport
        self._params_path = params_path
        self._output_path = (
            output_path
            if output_path is not None
            else os.environ[TEST_OUTDIR_ENV]
        )
        self._ffx_subtools_search_path = ffx_subtools_search_path

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
            - transport: fuchsia-controller
          TestParams:
            param_1: "val_1"
            param_2: "val_2"

        Args:
          transport: host->device transport type to use.

        Returns:
          A YAML string that represents a Mobly test config.
        """
        pass

    @abstractmethod
    def teardown(self, *args: Any) -> None:
        """Performs any required clean up upon Mobly test completion."""
        pass
