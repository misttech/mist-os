#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""CPU trace metrics."""

import logging
import statistics
from typing import Iterable, Iterator, Sequence

from trace_processing import trace_metrics, trace_model, trace_utils

_LOGGER: logging.Logger = logging.getLogger(__name__)
_CPU_USAGE_EVENT_NAME: str = "cpu_usage"


class CpuMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes the CPU utilization metrics."""

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates CpuMin, CpuMax, CpuAverage and CpuP* (%iles).
                Otherwise generates CpuLoad metric with all cpu values.
        """
        self.aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
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
            _LOGGER.info(
                "No cpu usage measurements are present. Perhaps the trace "
                "duration is too short to provide cpu usage information"
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
        return [
            trace_metrics.TestCaseResult(
                "CpuLoad", trace_metrics.Unit.percent, cpu_percentages
            )
        ]
