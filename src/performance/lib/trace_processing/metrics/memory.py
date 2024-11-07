#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Computes metrics from memory traces."""

import collections

from trace_processing import trace_metrics, trace_model, trace_utils

MEMORY_SYSTEM_CATEGORY = "memory:kernel"
KERNEL_EVENT_NAMES = ("kmem_stats_a", "kmem_stats_b", "kmem_stats_compression")


def standard_metrics_json(values: list[int | float]) -> trace_metrics.JSON:
    """Returns a json object holding the standard metric value keyed by metric name."""
    metrics = trace_utils.standard_metrics_set(
        values=values,
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
                    series_by_name[name].append(value)

        return (
            self.FREEFORM_METRICS_FILENAME,
            dict(
                kernel={
                    name: standard_metrics_json(series)
                    for name, series in series_by_name.items()
                }
            ),
        )
