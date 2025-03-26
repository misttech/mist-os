#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Computes metrics from memory traces."""

import collections

from trace_processing import trace_metrics, trace_model, trace_time, trace_utils

MEMORY_SYSTEM_CATEGORY = "memory:kernel"
KERNEL_EVENT_NAMES = (
    "kmem_stats_a",
    "kmem_stats_b",
    "kmem_stats_compression",
    "memory_stall",
)
# Name of the metric that are cumulative, monotonic counters, as opposed to gauges.
CUMULATIVE_METRIC_NAMES = {
    "compression_time",
    "decompression_time",
    "stall_time_some_ns",
    "stall_time_full_ns",
}


def safe_divide(numerator: float, denominator: float) -> float | None:
    """Divides numerator by denominator, returning None if denominator is 0."""
    if denominator == 0:
        return None
    else:
        return numerator / denominator


def cumulative_metrics_json(
    values: list[tuple[trace_time.TimePoint, int | float]]
) -> trace_metrics.JSON:
    """Returns a JSON object holding the change and the rate for the specified cumulative metric."""
    (t0, v0), (t1, v1) = values[0], values[-1]
    return {
        "Delta": v1 - v0,
        "Rate": safe_divide(v1 - v0, (t1 - t0).to_nanoseconds()),
    }


def gauges_metrics_json(
    values: list[tuple[trace_time.TimePoint, int | float]]
) -> trace_metrics.JSON:
    """Returns a JSON object holding the standard metric value keyed by metric name."""
    metrics = trace_utils.standard_metrics_set(
        values=list(v[1] for v in values),
        label_prefix="",
        unit=trace_metrics.Unit.bytes,
    )
    return {metric.label: metric.values[0] for metric in metrics}


class MemoryMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes statistics for values published by zircon kernel.

    Returns a freeform metric JSON object with nested structure with the following path:

    "kernel" / field name / statistic label / float value

    Where:
        kernel: constant "kernel"
        field name: name of a field from `zx_info_kmem_stats_extended` and
            `zx_info_kmem_stats_compression` structures.
        statistic name: label of the statistic published by `trace_utils.standard_metrics_set`.
        float value: metric value.

    Sample:

    {
        "kernel": {
            "total_bytes": {
                "Min": 112
                "P5": 130
                [...]
            } ,
            [...]
        }
    }
    """

    FREEFORM_METRICS_FILENAME = "memory"

    def process_freeform_metrics(
        self, model: trace_model.Model
    ) -> tuple[str, trace_metrics.JSON]:
        series_by_name = collections.defaultdict(list)
        for event in trace_utils.filter_events(
            model.all_events(),
            category=MEMORY_SYSTEM_CATEGORY,
            type=trace_model.CounterEvent,
        ):
            if event.name in KERNEL_EVENT_NAMES:
                for name, value in event.args.items():
                    series_by_name[name].append((event.start, value))

        return (
            self.FREEFORM_METRICS_FILENAME,
            dict(
                kernel={
                    name: cumulative_metrics_json(series)
                    if name in CUMULATIVE_METRIC_NAMES
                    else gauges_metrics_json(series)
                    for name, series in series_by_name.items()
                }
            ),
        )
