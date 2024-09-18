# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wakeup trace metrics."""

from typing import Sequence

from trace_processing import trace_metrics, trace_model, trace_time


class WakeupMetricsProcessor(trace_metrics.MetricsProcessor):
    """
    Computes time taken for the device to process a "wakeup", as defined by
    the provided series of trace events.
    """

    def __init__(self, label: str, event_names: list[str]) -> None:
        """
        Args:
            label: Report wakeup durations under this TestCaseResult label.
            event_names: Events that define a "wakeup".
        """
        self._label = label
        self._event_names = event_names

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        """Calculate Wakeup metrics.

        Args:
            model: In-memory representation of a system trace.

        Returns:
            Sequence with a TestCaseResult of Wakeup durations (or an empty
                list if no wakeup sequences were observed).
        """
        events = sorted(model.all_events(), key=lambda e: e.start)

        wakeup_start: trace_time.TimePoint | None = None
        wakeup_durations: list[float] = []
        next_event_index = 0

        for e in events:
            if e.name == self._event_names[0]:
                wakeup_start = e.start
                next_event_index = 1
            elif e.name == self._event_names[next_event_index]:
                if next_event_index == len(self._event_names) - 1:
                    assert wakeup_start is not None
                    wakeup_durations += [(e.start - wakeup_start)._delta]
                    wakeup_start = None
                    next_event_index = 0
                else:
                    next_event_index += 1

        return (
            [
                trace_metrics.TestCaseResult(
                    label=self._label,
                    unit=trace_metrics.Unit.nanoseconds,
                    values=wakeup_durations,
                )
            ]
            if wakeup_durations
            else []
        )
