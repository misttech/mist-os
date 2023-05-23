#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.tracing.py."""

import tempfile
import unittest

from honeydew.affordances.fuchsia_controller import tracing as fc_tracing


# pylint: disable=protected-access
class TracingFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.tracing.py."""

    def setUp(self) -> None:
        super().setUp()

        self.tracing_obj = fc_tracing.Tracing()

        self.assertIsInstance(self.tracing_obj, fc_tracing.Tracing)

    def test_initialize(self) -> None:
        """Test for Tracing.initialize() method."""
        with self.assertRaises(NotImplementedError):
            self.tracing_obj.initialize()

    def test_start(self) -> None:
        """Test for Tracing.start() method."""
        with self.assertRaises(NotImplementedError):
            self.tracing_obj.start()

    def test_stop(self) -> None:
        """Test for Tracing.stop() method."""
        with self.assertRaises(NotImplementedError):
            self.tracing_obj.stop()

    def test_terminate(self) -> None:
        """Test for Tracing.terminate() method."""
        with self.assertRaises(NotImplementedError):
            self.tracing_obj.terminate()

    def test_terminate_and_download(self) -> None:
        """Test for TracingDefault.terminate_and_download() method."""
        with tempfile.TemporaryDirectory() as tmpdir:
            with self.assertRaises(NotImplementedError):
                self.tracing_obj.terminate_and_download(directory=tmpdir)


if __name__ == "__main__":
    unittest.main()
