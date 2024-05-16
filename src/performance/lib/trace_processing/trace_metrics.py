#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Metrics processing common code for trace models.

This module implements the perf test results schema.

See https://fuchsia.dev/fuchsia-src/development/performance/fuchsiaperf_format
for more details.
"""

import abc
import dataclasses
import enum
import json
import logging
import pathlib
from typing import Any, Iterable, Sequence

import trace_processing.trace_model as trace_model


_LOGGER: logging.Logger = logging.getLogger("Performance")


class Unit(enum.StrEnum):
    """The set of valid Unit constants.

    This should be kept in sync with the list of supported units in the results
    schema docs linked at the top of this file. These are the unit strings
    accepted by catapult_converter.
    """

    # Time-based units.
    nanoseconds = "nanoseconds"
    milliseconds = "milliseconds"
    # Size-based units.
    bytes = "bytes"
    bytesPerSecond = "bytes/second"
    # Frequency-based units.
    framesPerSecond = "frames/second"
    # Percentage-based units.
    percent = "percent"
    # Count-based units.
    countSmallerIsBetter = "count_smallerIsBetter"
    countBiggerIsBetter = "count_biggerIsBetter"
    # Power-based units.
    watts = "Watts"


@dataclasses.dataclass(frozen=True)
class TestCaseResult:
    """The results for a single test case.

    See the link at the top of this file for documentation.
    """

    label: str
    unit: Unit
    values: list[float]

    def to_json(self, test_suite: str) -> dict[str, Any]:
        return {
            "label": self.label,
            "test_suite": test_suite,
            "unit": str(self.unit),
            "values": self.values,
        }

    @staticmethod
    def write_fuchsiaperf_json(
        results: Iterable["TestCaseResult"],
        test_suite: str,
        output_path: pathlib.Path,
    ) -> None:
        """Writes the given TestCaseResults into a fuchsiaperf json file.

        Args:
            results: The results to write.
            test_suite: A test suite name to embed in the json.
                E.g. "fuchsia.uiperf.my_metric".
            output_path: Output file path, must end with ".fuchsiaperf.json".
        """
        assert output_path.name.endswith(
            ".fuchsiaperf.json"
        ), f"Expecting path that ends with '.fuchsiaperf.json' but got {output_path}"
        results_json = [r.to_json(test_suite) for r in results]
        with open(output_path, "w") as outfile:
            json.dump(results_json, outfile, indent=4)
        _LOGGER.info(f"Wrote {len(results_json)} results into {output_path}")


class MetricsProcessor(abc.ABC):
    """MetricsProcessor converts a trace_model.Model into TestCaseResults.

    This abstract class is extended to implement various types of metrics.

    MetricsProcessor subclasses can be used as follows:

    ```
    processor = MetricsProcessorSet([
      CpuMetricsProcessor(aggregates_only=True),
      FpsMetricsProcessor(aggregates_only=False),
      MyCustomProcessor(...),
      power_sampler.metrics_processor(),
    ])

    ... gather traces, start and stop the power sampler, create the model ...

    processor.process_and_save(model, output_path="my_test.fuchsiaperf.json")
    ```
    """

    @property
    def name(self) -> str:
        return self.__class__.__name__

    @abc.abstractmethod
    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[TestCaseResult]:
        """Generates metrics from the given model.

        Args:
            model: The input trace model.

        Returns:
            List[TestCaseResult]: The generated metrics.
        """

    def process_and_save_metrics(
        self,
        model: trace_model.Model,
        test_suite: str,
        output_path: pathlib.Path,
    ) -> None:
        """Convenience method for processing a model and saving the output into a fuchsiaperf.json file.

        Args:
            model: A model to process metrics from.
            test_suite: A test suite name to embed in the json.
                E.g. "fuchsia.uiperf.my_metric".
            output_path: Output file path, must end with ".fuchsiaperf.json".
        """
        results = self.process_metrics(model)
        TestCaseResult.write_fuchsiaperf_json(
            results, test_suite=test_suite, output_path=output_path
        )


class ConstantMetricsProcessor(MetricsProcessor):
    """A metrics processor that return a constant list of result."""

    def __init__(self, results: Sequence[TestCaseResult]):
        self.results: Sequence[TestCaseResult] = results

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[TestCaseResult]:
        return self.results


class MetricsProcessorsSet(MetricsProcessor):
    """A processor that aggregates N sub-processors."""

    def __init__(self, sub_processors: Sequence[MetricsProcessor]):
        self.sub_processors: Sequence[MetricsProcessor] = sub_processors

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[TestCaseResult]:
        results: list[TestCaseResult] = []
        _LOGGER.info(
            f"Combining metrics from {len(self.sub_processors)} subprocessors..."
        )
        for sub_proc in self.sub_processors:
            sub_results = sub_proc.process_metrics(model)
            _LOGGER.info(
                f"Got {len(sub_results)} results from subprocessor {sub_proc.name}"
            )
            results.extend(sub_results)
        return results
