#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Benchmarks for the Diagnostics Selectors library."""

import perf_test
from mobly import test_runner

_PACKAGE = "selectors-benchmarks"


class SelectorsBenchmarks(perf_test.FuchsiaPerfTest):
    """Diagnostics selectors benchmarks tests"""

    def test_selectors_benchmarks(self) -> None:
        """Test case that runs the Selectors library benchmarks."""
        self.run_test_component(
            _PACKAGE, "selectors-benchmarks.cm", "fuchsia.diagnostics.txt"
        )


if __name__ == "__main__":
    test_runner.main()
