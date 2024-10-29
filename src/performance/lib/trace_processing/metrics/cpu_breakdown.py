#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from trace_processing import trace_metrics, trace_model
from trace_processing.metrics import cpu


# TODO(b/375257864): Delete this file once cross-repo deps are migrated to cpu
class CpuBreakdownMetricsProcessor(trace_metrics.MetricsProcessor):
    """
    Breaks down CPU metrics into a free-form metrics format.
    """

    def __init__(
        self,
        percent_cutoff: float = cpu._DEFAULT_PERCENT_CUTOFF,
    ) -> None:
        self._processor = cpu.CpuMetricsProcessor(percent_cutoff=percent_cutoff)

    def process_freeform_metrics(
        self, model: trace_model.Model
    ) -> list[dict[str, trace_metrics.JsonType]]:
        """
        Given trace_model.Model, iterates through all the SchedulingRecords and calculates the
        duration for each Process's Threads, and saves them by CPU.

        Args:
            model: The input trace model.

        Returns:
            list[dict[str, trace_metrics.JsonType]]: Data structure of metrics that can be directly
                                                     converted to JSON.
        """
        (breakdown, _) = self.process_metrics_and_get_total_time(model)
        return breakdown

    def process_metrics_and_get_total_time(
        self, model: trace_model.Model
    ) -> tuple[list[dict[str, trace_metrics.JsonType]], float]:
        """
        Given trace_model.Model, iterates through all the SchedulingRecords and calculates the
        duration for each Process's Threads, and saves them by CPU.

        Args:
            model: The input trace model.

        Returns:
            list[dict[str, trace_metrics.JsonType]]: Data structure of metrics that can be directly
                                                     converted to JSON.
            float: The total duration of the trace.
        """
        return self._processor.process_metrics_and_get_total_time(model)


def group_by_process_name(
    breakdown: list[dict[str, trace_metrics.JsonType]]
) -> list[dict[str, trace_metrics.JsonType]]:
    """
    Given a breakdown, group the metrics by process_name only,
    ignoring thread name.
    """
    return cpu.group_by_process_name(breakdown)
