#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Benchmarks for the Rust diagnostics_log library."""

import perf_test
from mobly import test_runner

_PACKAGE = "diagnostics-log-rust-benchmarks"


class DiagnosticsLogRustBenchmarks(perf_test.FuchsiaPerfTest):
    """Rust diagnostics_log lib benchmarks tests"""

    def test_core_benchmarks(self) -> None:
        """Test case that runs the Rust diagnostics_log lib core benchmarks."""
        self.run_test_component(
            _PACKAGE, "core.cm", "fuchsia.diagnostics_log_rust.core.txt"
        )

    def test_encoding_benchmarks(self) -> None:
        """Test case that runs the Rust diagnostics_log lib encoding benchmarks."""
        self.run_test_component(
            _PACKAGE, "encoding.cm", "fuchsia.diagnostics_log_rust.encoding.txt"
        )


if __name__ == "__main__":
    test_runner.main()
