# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Provides an abstraction to assert that a set of metrics matches the expected one."""

import os

_OPTIONAL_SUFFIX: str = " [optional]"


class MetricsAllowlist:
    def __init__(self, file_path: str | os.PathLike[str]):
        """Creates a new metric allowlist.

        Args:
          file_path: path to the file containing the expected metrics.
        """
        self.file_path = file_path
        self.optional_metrics: set[str] = set()
        self.expected_metrics: set[str] = set()
        self._should_summarize = True

        allow_command = True
        with open(self.file_path) as f:
            for line in f:
                line = line.rstrip("\n")
                if line.strip() == "[no-summarize-metrics]":
                    if not allow_command:
                        raise ValueError(
                            "[no-summarize-metrics] can only appear at the beginning of the file"
                        )
                    self._should_summarize = False
                    continue
                if line.strip().startswith("#") or line.strip() == "":
                    continue
                if line.endswith(_OPTIONAL_SUFFIX):
                    self.optional_metrics.add(
                        line.removesuffix(_OPTIONAL_SUFFIX)
                    )
                else:
                    self.expected_metrics.add(line)
                allow_command = False

    @property
    def should_summarize(self) -> bool:
        return self._should_summarize

    def check(self, actual_metrics: set[str]) -> None:
        """Checks that the given `actual_metrics` matches the expected metrics.

        Args:
          actual_metrics: the set of metrics to check.

        Raises:
          ValueError: when the metrics don't match. Includes the diff.
        """
        actual_minus_optional: set[str] = actual_metrics.difference(
            self.optional_metrics
        )
        if self.expected_metrics == actual_minus_optional:
            return
        union: list[str] = sorted(
            actual_metrics.union(self.expected_metrics).union(
                self.optional_metrics
            )
        )
        lines: list[str] = []
        for entry in union:
            prefix: str = " "
            suffix: str = ""
            if entry in self.optional_metrics:
                suffix = _OPTIONAL_SUFFIX
            elif entry in actual_metrics:
                if entry not in self.expected_metrics:
                    prefix = "+"
            else:
                prefix = "-"
            lines.append(prefix + entry + suffix)
        diff = "\n".join(lines)

        raise ValueError(
            (
                f"Metric names produced by the test differ from the expectations in "
                f"{self.file_path}: {diff}\n\n"
                "One way to update the expectation file is to run the test locally with this "
                "environment variable set:\n"
                "FUCHSIA_EXPECTED_METRIC_NAMES_DEST_DIR="
                "$(pwd)/src/tests/end_to_end/perf/expected_metric_names\n\n"
                "See https://fuchsia.dev/fuchsia-src/development/performance/metric_name_expectations"
            )
        )
