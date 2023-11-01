#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""CPU trace metrics."""

import logging
from typing import Any, Dict, Iterable, Iterator, List

import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils


_LOGGER: logging.Logger = logging.getLogger("CpuMetricsProcessor")
_CPU_USAGE_EVENT_NAME: str = "cpu_usage"
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"


def metrics_processor(
    model: trace_model.Model, extra_args: Dict[str, Any]
) -> List[trace_metrics.TestCaseResult]:
    """Computes the CPU utilization for the given trace.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """

    all_events: Iterator[trace_model.Event] = model.all_events()
    cpu_usage_events: Iterable[trace_model.Event] = trace_utils.filter_events(
        all_events,
        category="system_metrics_logger",
        name=_CPU_USAGE_EVENT_NAME,
        type=trace_model.CounterEvent,
    )
    cpu_percentages: List[float] = list(
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
            f"{duration.toMilliseconds()} milliseconds) is too short to provide"
            f"cpu usage information"
        )
        return []

    cpu_mean: float = trace_utils.mean(cpu_percentages)
    _LOGGER.info(f"Average CPU Load: {cpu_mean}")

    test_case_results: List[trace_metrics.TestCaseResult] = []
    if extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True:
        for percentile in [5, 25, 50, 75, 95]:
            cpu_percentile: float = trace_utils.percentile(
                cpu_percentages, percentile
            )
            test_case_results.append(
                trace_metrics.TestCaseResult(
                    f"cpu_p{percentile}",
                    trace_metrics.Unit.percent,
                    [cpu_percentile],
                )
            )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"cpu_min", trace_metrics.Unit.percent, [min(cpu_percentages)]
            )
        )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"cpu_max", trace_metrics.Unit.percent, [max(cpu_percentages)]
            )
        )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"cpu_average", trace_metrics.Unit.percent, [cpu_mean]
            )
        )
    else:
        result: trace_metrics.TestCaseResult = trace_metrics.TestCaseResult(
            "CPU Load", trace_metrics.Unit.percent, cpu_percentages
        )
        test_case_results.append(result)

    return test_case_results
