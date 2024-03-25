#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""CPU trace metrics."""

import logging
from typing import Any, Iterable, Iterator

import statistics
import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils

_LOGGER: logging.Logger = logging.getLogger(__name__)
_CPU_USAGE_EVENT_NAME: str = "cpu_usage"
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"


# DEPRECATED: Use CpuMetricsProcessor instead.
# TODO(b/320778225): Remove once downstream callers are migrated.
def metrics_processor(
    model: trace_model.Model, extra_args: dict[str, Any]
) -> list[trace_metrics.TestCaseResult]:
    """Computes the CPU utilization for the given trace.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """
    return CpuMetricsProcessor(
        aggregates_only=extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True
    ).process_metrics(model)


class CpuMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes the CPU utilization metrics."""

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates CpuMin, CpuMax, CpuAverage and CpuP* (percentiles).
                Otherwise generates CpuLoad metric with all cpu values.
        """
        self.aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> list[trace_metrics.TestCaseResult]:
        all_events: Iterator[trace_model.Event] = model.all_events()
        cpu_usage_events: Iterable[
            trace_model.Event
        ] = trace_utils.filter_events(
            all_events,
            category="system_metrics_logger",
            name=_CPU_USAGE_EVENT_NAME,
            type=trace_model.CounterEvent,
        )
        cpu_percentages: list[float] = list(
            trace_utils.get_arg_values_from_events(
                cpu_usage_events,
                arg_key=_CPU_USAGE_EVENT_NAME,
                arg_types=(int, float),
            )
        )

        # TODO(b/156300857): Remove this fallback after all consumers have been
        # updated to use system_metrics_logger.
        if len(cpu_percentages) == 0:
            all_events = model.all_events()
            cpu_usage_events = trace_utils.filter_events(
                all_events,
                category="system_metrics",
                name=_CPU_USAGE_EVENT_NAME,
                type=trace_model.CounterEvent,
            )
            cpu_percentages = list(
                trace_utils.get_arg_values_from_events(
                    cpu_usage_events,
                    arg_key="average_cpu_percentage",
                    arg_types=(int, float),
                )
            )

        if len(cpu_percentages) == 0:
            duration: trace_time.TimeDelta = model.total_duration()
            _LOGGER.info(
                f"No cpu usage measurements are present. Perhaps the trace duration"
                f"{duration.to_milliseconds()} milliseconds) is too short to provide"
                f"cpu usage information"
            )
            return []

        cpu_mean: float = statistics.mean(cpu_percentages)
        _LOGGER.info(f"Average CPU Load: {cpu_mean}")

        if self.aggregates_only:
            return trace_utils.standard_metrics_set(
                values=cpu_percentages,
                label_prefix="Cpu",
                unit=trace_metrics.Unit.percent,
            )
        else:
            return [
                trace_metrics.TestCaseResult(
                    "CpuLoad", trace_metrics.Unit.percent, cpu_percentages
                )
            ]
