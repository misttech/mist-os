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
from typing import Any, Iterable, Mapping, Sequence, TypeAlias

from trace_processing import trace_model

JSON: TypeAlias = (
    Mapping[str, "JSON"] | Sequence["JSON"] | str | int | float | bool | None
)

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
    values: tuple[float, ...]

    def __init__(self, label: str, unit: Unit, values: Sequence[float]):
        """Allows any Sequence to be used for values while staying hashable."""
        object.__setattr__(self, "label", label)
        object.__setattr__(self, "unit", unit)
        object.__setattr__(self, "values", tuple(values))

    def to_json(self, test_suite: str) -> dict[str, Any]:
        return {
            "label": self.label,
            "test_suite": test_suite,
            "unit": str(self.unit),
            "values": list(self.values),
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

    TestCaseResult.write_fuchsiaperf_json(
        processor.process_metrics(model), test_suite_name, output_path
    )
    ```
    NB: `output_path` must end in `fuchsiaperf.json`
    """

    @property
    def name(self) -> str:
        return self.__class__.__name__

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[TestCaseResult]:
        """Generates metrics from the given model.

        Args:
            model: The input trace model.

        Returns:
            list[TestCaseResult]: The generated metrics.
        """
        return []

    def process_freeform_metrics(
        self, model: trace_model.Model
    ) -> tuple[str, JSON]:
        """Computes freeform metrics as JSON.

        This can output structured data, as opposite to `process_metrics` which return as list.
        These metrics are in addition to those produced by process_metrics()

        This method returns a tuple of (filename, JSON) so that processors can provide an
        identifier more stable than its own classname for use when filing freeform metrics. Since
        filenames are included when freeform metrics are ingested into the metrics backend, basing
        that name on a class name would mean that a refactor could unintentionally break downstream
        consumers of metrics.

        Args:
            model: trace events to be processed.

        Returns:
            str: stable identifier to use in freeform metrics file name.
            JSON: structure holding aggregated metrics, or None if not supported.
        """
        return (self.name, None)


class ConstantMetricsProcessor(MetricsProcessor):
    """A metrics processor that returns constant results.

    Enables publishing of metrics gathered via means other than trace processing.
    """

    def __init__(
        self,
        metrics: Sequence[TestCaseResult] = (),
        freeform_metrics: tuple[str, JSON] = ("", None),
    ):
        self.metrics = metrics
        self.freeform_metrics = freeform_metrics

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[TestCaseResult]:
        return self.metrics

    def process_freeform_metrics(
        self, model: trace_model.Model
    ) -> tuple[str, JSON]:
        return self.freeform_metrics
