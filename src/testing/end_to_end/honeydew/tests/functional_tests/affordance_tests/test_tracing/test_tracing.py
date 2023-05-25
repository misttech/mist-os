#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Tracing affordance."""

import logging
import os
import tempfile
import time

from fuchsia_base_test import fuchsia_base_test
from honeydew.interfaces.device_classes import fuchsia_device
from honeydew.interfaces.device_classes import tracing_capable_device
from mobly import asserts
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class TracingAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Tracing affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

    def test_is_device_tracing_capable(self) -> None:
        """Test case to make sure DUT is a tracing capable device"""
        asserts.assert_is_instance(
            self.device, tracing_capable_device.TracingCapableDevice)

    # Mobly enumerates test cases alphabetically, change in order of test cases
    # or their names or mobly enumeration logic can break tests. To avoid this,
    # we call all dependent operations in a single test method.
    def test_tracing_terminate(self) -> None:
        """Test case for all tracing methods.

        This test case calls the following tracing methods:
                * `tracing.initialize()`
                * `tracing.start()`
                * `tracing.stop()`
                * `tracing.terminate()`
        """
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.tracing.initialize()
                self.device.tracing.start()
                self.device.tracing.stop()
                self.device.tracing.terminate()
            return

        # Initialize Tracing Session.
        self.device.tracing.initialize()

        # Start Tracing.
        self.device.tracing.start()

        # Stop Tracing.
        self.device.tracing.stop()

        # Terminate the tracing session.
        self.device.tracing.terminate()

    def test_tracing_trace_download(self) -> None:
        """ This test case tests the following tracing methods and asserts that
            the trace was downloaded successfully.

        This test case calls the following tracing methods:
                * `tracing.initialize()`
                * `tracing.start()`
                * `tracing.stop()`
                * `tracing.terminate_and_download(directory="/tmp/")`
        """
        if self._is_fuchsia_controller_based_device(self.device):
            with asserts.assert_raises(NotImplementedError):
                self.device.tracing.terminate_and_download(
                    directory="/tmp", trace_file="trace.fxt")
            return

        # Initialize Tracing Session.
        self.device.tracing.initialize()

        # Start Tracing.
        self.device.tracing.start()

        time.sleep(1)

        # Stop Tracing.
        self.device.tracing.stop()

        # Terminate the tracing session.
        with tempfile.TemporaryDirectory() as tmpdir:
            res = self.device.tracing.terminate_and_download(
                directory=tmpdir, trace_file="trace.fxt")

            asserts.assert_equal(
                res, f"{tmpdir}/trace.fxt", msg="trace not downloaded")
            asserts.assert_true(
                os.path.exists(f"{tmpdir}/trace.fxt"), msg="trace failed")


if __name__ == "__main__":
    test_runner.main()
