#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Benchmarks for the Archivist flows."""

import perf_test
from mobly import test_runner

_PACKAGE = "archivist-benchmarks"


class ArchivistBenchmarks(perf_test.FuchsiaPerfTest):
    """Archivist benchmark tests"""

    def test_logging_benchmarks(self) -> None:
        """Test case that runs Archivist logging benchmarks."""
        self.run_test_component(
            _PACKAGE, "logging.cm", "fuchsia.archivist.logging.txt"
        )

    def test_formatter_benchmarks(self) -> None:
        """Test case that runs Archivist formatter benchmarks."""
        self.run_test_component(
            _PACKAGE, "formatter.cm", "fuchsia.archivist.formatter.txt"
        )


if __name__ == "__main__":
    test_runner.main()
