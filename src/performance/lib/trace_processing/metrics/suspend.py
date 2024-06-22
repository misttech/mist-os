# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Suspend trace metrics."""

from typing import Iterable, Sequence

from trace_processing import (
    trace_metrics,
    trace_model,
    trace_time,
    trace_utils,
)


class SuspendMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes suspend/resume metrics."""

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        """Calculate suspend/resume metrics.

        Args:
            model: In-memory representation of a merged power and system trace.

        Returns:
            Set of metrics results for this test case.
        """

        suspend_events: Iterable[
            trace_model.DurationEvent
        ] = trace_utils.filter_events(
            model.all_events(),
            category="power",
            name="suspend",
            type=trace_model.DurationEvent,
        )
        suspend_time = trace_time.TimeDelta(0)
        for se in suspend_events:
            if se.duration is not None:
                suspend_time += se.duration

        total_time = trace_utils.total_event_duration(model.all_events())
        running_time = total_time - suspend_time

        return [
            trace_metrics.TestCaseResult(
                label="UnsuspendedTime",
                unit=trace_metrics.Unit.nanoseconds,
                values=[running_time.to_nanoseconds()],
            ),
            trace_metrics.TestCaseResult(
                label="SuspendTime",
                unit=trace_metrics.Unit.nanoseconds,
                values=[suspend_time.to_nanoseconds()],
            ),
            trace_metrics.TestCaseResult(
                label="SuspendPercentage",
                unit=trace_metrics.Unit.percent,
                values=[(suspend_time / total_time) * 100],
            ),
        ]
