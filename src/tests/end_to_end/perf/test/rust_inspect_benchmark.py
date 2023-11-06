#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Benchmarks for the Rust Inspect library."""

import perf_test
from mobly import test_runner

_PACKAGE = "rust-inspect-benchmarks"


class RustInspectBenchmarks(perf_test.FuchsiaPerfTest):
    """Rust inspect benchmarks tests"""

    def test_reader_benchmarks(self) -> None:
        """Test case that runs the Rust Inspect library reader benchmarks."""
        self.run_test_component(
            _PACKAGE, "reader.cm", "fuchsia.rust_inspect.reader_benchmarks.txt"
        )

    def test_writer_benchmarks(self) -> None:
        """Test case that runs the Rust Inspect library writer benchmarks."""
        self.run_test_component(
            _PACKAGE, "writer.cm", "fuchsia.rust_inspect.benchmarks.txt"
        )

    def test_snapshot_filter_benchmarks(self) -> None:
        """Test case that runs the Rust Inspect library snapshot and filter benchmarks."""
        self.run_test_component(
            _PACKAGE, "snapshot_filter.cm", "fuchsia.rust_inspect.selectors.txt"
        )


if __name__ == "__main__":
    test_runner.main()
