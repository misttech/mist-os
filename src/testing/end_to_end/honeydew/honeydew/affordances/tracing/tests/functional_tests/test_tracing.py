# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Mobly test for Tracing affordance."""

import json
import logging
import os
import subprocess
import tempfile
import time

from fuchsia_base_test import fuchsia_base_test
from mobly import asserts, test_runner

from honeydew.interfaces.device_classes import fuchsia_device

_LOGGER: logging.Logger = logging.getLogger(__name__)
# This path is generated from host_test_data deps defined in this folder's BUILD.gn
TRACE2JSON = "trace_runtime_deps/trace2json"


class TracingAffordanceTests(fuchsia_base_test.FuchsiaBaseTest):
    """Tracing affordance tests"""

    def setup_class(self) -> None:
        """setup_class is called once before running tests.

        It does the following things:
            * Assigns `device` variable with FuchsiaDevice object
        """
        super().setup_class()
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]

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
        # Initialize Tracing Session.
        self.device.tracing.initialize()

        # Start Tracing.
        self.device.tracing.start()

        # Stop Tracing.
        self.device.tracing.stop()

        # Terminate the tracing session.
        self.device.tracing.terminate()

    def test_tracing_trace_download(self) -> None:
        """This test case tests the following tracing methods and asserts that
            the trace was downloaded successfully.

        This test case calls the following tracing methods:
                * `tracing.initialize()`
                * `tracing.start()`
                * `tracing.stop()`
                * `tracing.terminate_and_download(directory="/tmp/")`
        """
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
                directory=tmpdir, trace_file="trace.fxt"
            )

            asserts.assert_equal(
                res, f"{tmpdir}/trace.fxt", msg="trace not downloaded"
            )
            asserts.assert_true(
                os.path.exists(f"{tmpdir}/trace.fxt"), msg="trace failed"
            )
            ps = subprocess.Popen(
                [TRACE2JSON], stdin=subprocess.PIPE, stdout=subprocess.PIPE
            )
            trace_data: bytes = bytes([])
            with open(res, "rb") as trace_file:
                trace_data = trace_file.read()
            js, _ = ps.communicate(input=trace_data)
            # Asserts this can be converted into valid JSON.
            js_obj = json.loads(js.decode("utf8"))
            ps.kill()
            asserts.assert_true(
                js_obj.get("traceEvents") is not None,
                "Expected traceEvents to be present",
            )
            # The general schema of the trace file looks like:
            #
            # {
            #   'displayTimeUnit': #TIME_UNIT (usually 'ns'),
            #   'traceEvents': [ #TRACE_EVENT ]
            # }
            #
            # Trace event is defined (roughly) as:
            #
            # {
            #   'cat': #CATEGORY,
            #   'name': #NAME,
            #   'ts': #TIMESTAMP
            #   'pid': #PID,
            #   'tid': #TID,
            #   ...
            # }
            events = js_obj["traceEvents"]
            asserts.assert_true(
                len(events) > 0,
                "Expected at least one captured trace event",
            )

    def test_tracing_session(self) -> None:
        """This test case tests the `tracing.trace_session()` context manager"""
        with self.device.tracing.trace_session():
            pass

    def test_tracing_session_download(self) -> None:
        """This test case tests the `tracing.trace_session()` context manager
        and asserts that the trace was downloaded successfully.
        """
        with tempfile.TemporaryDirectory() as tmpdir:
            with self.device.tracing.trace_session(
                download=True, directory=tmpdir, trace_file="trace.fxt"
            ):
                pass
            asserts.assert_true(
                os.path.exists(f"{tmpdir}/trace.fxt"), msg="trace failed"
            )

    def test_multi_tracing_session(self) -> None:
        """This test case tests the multiple traces using trace context manager"""
        with self.device.tracing.trace_session():
            self.device.tracing.stop()
            time.sleep(1)
            self.device.tracing.start()


if __name__ == "__main__":
    test_runner.main()
