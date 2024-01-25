#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Metrics processing common code for trace models.

This module implements the perf test results schema.

See https://fuchsia.dev/fuchsia-src/development/performance/fuchsiaperf_format
for more details.
"""

from enum import Enum
import json
import logging
import os
from typing import Any, Callable, Dict, List, Optional, TextIO, Union

import trace_processing.trace_model as trace_model


_LOGGER: logging.Logger = logging.getLogger("Performance")


class Unit(Enum):
    """The set of valid Unit constants.

    This should be kept in sync with the list of supported units in the results
    schema docs linked at the top of this file.
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


class TestCaseResult:
    """The results for a single test case.

    See the link at the top of this file for documentation.
    """

    # Maps a [Unit] to the equivalent string expected in catapult converter.
    _UNIT_TO_CONVERT_STRING: Dict[Unit, str] = {
        Unit.nanoseconds: "nanoseconds",
        Unit.milliseconds: "milliseconds",
        Unit.bytes: "bytes",
        Unit.bytesPerSecond: "bytes/second",
        Unit.framesPerSecond: "frames/second",
        Unit.percent: "percent",
        Unit.countSmallerIsBetter: "count_smallerIsBetter",
        Unit.countBiggerIsBetter: "count_biggerIsBetter",
        Unit.watts: "Watts",
    }

    def __init__(self, metric: str, unit: Unit, values: List[float]) -> None:
        self.metric: str = metric
        # This field below is being renamed from "label" to "metric".
        # It is duplicated as a transitional measure so that it can be accessed
        # via either name.  TODO(https://fxbug.dev/42137976): Remove the "label" field."
        self.label: str = metric
        self.unit: Unit = unit
        self.values: List[float] = values

        # TODO(https://fxbug.dev/42137976): Remove the statement below when "label" is removed.
        self.label = metric

    def to_json(self, test_suite: str) -> Dict[str, Any]:
        convert_string: Optional[str] = self._UNIT_TO_CONVERT_STRING.get(
            self.unit
        )
        if convert_string is None:
            raise ValueError(
                f"Failed to map {self.unit} to catapult converter string"
            )
        return {
            "label": self.label,
            "test_suite": test_suite,
            "unit": convert_string,
            "values": self.values,
        }


# A callable object which can extract a set of metrics from a trace [Model],
# encoded into a [TestCaseResult].
MetricsProcessor = Callable[
    [trace_model.Model, Dict[str, Any]], List[TestCaseResult]
]


class MetricsSpec:
    """A specification of a metric."""

    def __init__(
        self,
        name: str,
        processor: MetricsProcessor,
        extra_args: Optional[Dict[str, Any]] = None,
    ) -> None:
        self.name: str = name
        self.processor: MetricsProcessor = processor
        self.extra_args: Dict[str, Any] = extra_args or {}

    def process_metrics(self, model: trace_model.Model) -> List[TestCaseResult]:
        """Processes the given Model according to this metrics spec.

        Args:
        model: The model to process the metrics against.

        Returns:
        A list of test case results for the processed metrics.
        """
        return self.processor(model, self.extra_args)


class MetricsSpecSet:
    """A collection of [MetricsSpec]s."""

    def __init__(
        self,
        specs: List[MetricsSpec],
        test_suite: Optional[str] = None,
        test_name: Optional[str] = None,
    ) -> None:
        self.metrics_specs: List[MetricsSpec] = specs
        self.test_suite: Optional[str] = test_suite
        self.test_name: Optional[str] = test_name

        # TODO(https://fxbug.dev/42137976): Remove the if block below, which is used for
        # backward compatible transition purpose.
        if self.test_name is None and self.test_suite is not None:
            self.test_name = self.test_suite

    def process_trace(
        self,
        model: trace_model.Model,
        output_dir: Union[str, os.PathLike],
    ) -> str:
        """Runs the provided |metrics_spec_set| on a trace [Model], generating a
        file in the fuchsiaperf.json format.

        Args:
          metrics_spec_set: The metrics spec set to run over the |model|.
          model: The tracing model to process.
          output_path: Directory to place the generated fuchsiaperf.json file.

        Returns:
          The fuchiaperf.json file generated by trace processing.
        """

        _LOGGER.info(f"Processing trace.")
        output_file_name = os.path.join(
            str(output_dir),
            f"{self.test_name}-benchmark.fuchsiaperf.json",
        )

        results: List[Dict[str, Any]] = []
        for metrics_spec in self.metrics_specs:
            _LOGGER.info(f"Applying metrics spec {metrics_spec.name} to model")
            test_case_results: List[
                TestCaseResult
            ] = metrics_spec.process_metrics(model)
            for test_case_result in test_case_results:
                results.append(
                    test_case_result.to_json(test_suite=self.test_name)
                )

        with open(output_file_name, "w") as output_file:
            json.dumps(results, output_file)
        _LOGGER.info(
            f"Processing trace completed; written to " f"{output_file_name}."
        )

        return output_file_name
