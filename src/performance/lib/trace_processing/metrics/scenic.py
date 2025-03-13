#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Scenic trace metrics."""

import logging
import statistics
from typing import Iterator, Sequence, Tuple

from trace_processing import trace_metrics, trace_model, trace_utils

_LOGGER: logging.Logger = logging.getLogger("ScenicMetricsProcessor")
_EVENT_CATEGORY: str = "gfx"
_SCENIC_START_EVENT_NAME: str = "ApplyScheduledSessionUpdates"
_SCENIC_RENDER_EVENT_NAME: str = "RenderFrame"
_DISPLAY_VSYNC_READY_EVENT_NAME: str = "Display::Fence::OnReady"


class _ScenicTracingEvent:
    start_event: trace_model.Event
    render_event: trace_model.DurationEvent
    vsync_event: trace_model.Event

    def __init__(
        self,
        start_event: trace_model.Event,
        render_event: trace_model.DurationEvent,
        vsync_event: trace_model.Event,
    ):
        self.start_event = start_event
        self.render_event = render_event
        self.vsync_event = vsync_event


class ScenicMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes CPU and GPU time spent rendering frames in Scenic.

    Calculates total time-to-render-frame by measuring from the moment that Scenic reports it has
    begun computing frame contents to the moment that is ready for vsync. Also tracks time spent on
    CPU-bound operations for each frame. Flow events in the trace enable this class to reliably
    correlate the correct events to calculate this duration.

    By default, this module reports aggregate latency measurements -- such as min, max, average, and
    percentiles -- calculated across all frames rendered during the test. It can be
    configured to instead report a time series of measurements, one for each event.
    """

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates RenderCpu[Min|Max|Average|P*] and
                RenderTotal[Min|Max|Average|P*].
                Otherwise generates RenderCpu and RenderTotal with the raw values.
        """
        self.aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> Sequence[trace_metrics.TestCaseResult]:
        # This method looks for a possible race between trace event start in Scenic and magma.
        # We can safely skip these events. See https://fxbug.dev/322849857 for more details.
        model = trace_utils.adjust_to_common_process_start(
            model,
            _SCENIC_START_EVENT_NAME,
            category=_EVENT_CATEGORY,
            type=trace_model.DurationEvent,
        )

        all_events: Iterator[trace_model.Event] = model.all_events()
        # Since `filter_events()` returns an Iterator, make a local copy so we can iterate over the
        # events more than once.
        scenic_start_events = list(
            trace_utils.filter_events(
                all_events,
                category=_EVENT_CATEGORY,
                name=_SCENIC_START_EVENT_NAME,
                type=trace_model.DurationEvent,
            )
        )
        tracing_events: list[_ScenicTracingEvent] = []
        for e in scenic_start_events:
            render_event = trace_utils.get_nearest_following_event(
                e, _EVENT_CATEGORY, _SCENIC_RENDER_EVENT_NAME
            )
            if render_event is None:
                continue
            assert isinstance(render_event, trace_model.DurationEvent)
            if not render_event.duration:
                continue
            vsync_ready_event = trace_utils.get_nearest_following_event(
                render_event, _EVENT_CATEGORY, _DISPLAY_VSYNC_READY_EVENT_NAME
            )
            if vsync_ready_event is None:
                continue
            tracing_events.append(
                _ScenicTracingEvent(e, render_event, vsync_ready_event)
            )

        if len(tracing_events) < 1:
            _LOGGER.info(
                "No render or vsync events are present. Perhaps the trace "
                "duration is too short to provide scenic render information"
            )
            return []

        cpu_render_times: list[float] = []
        for tracing_event in tracing_events:
            if tracing_event.render_event.duration is None:
                continue
            cpu_render_times.append(
                (
                    tracing_event.render_event.start
                    + tracing_event.render_event.duration
                    - tracing_event.start_event.start
                ).to_milliseconds_f()
            )

        cpu_render_mean: float = statistics.mean(cpu_render_times)
        _LOGGER.info(f"Average CPU render time: {cpu_render_mean} ms")

        total_render_times: list[float] = []
        for tracing_event in tracing_events:
            (
                tracing_event.vsync_event.start
                - tracing_event.start_event.start
            ).to_milliseconds_f()
            total_render_times.append(
                (
                    tracing_event.vsync_event.start
                    - tracing_event.start_event.start
                ).to_milliseconds_f()
            )

        total_render_mean: float = statistics.mean(total_render_times)
        _LOGGER.info(f"Average Total render time: {total_render_mean} ms")

        metrics_list: list[Tuple[str, list[float]]] = [
            ("RenderCpu", cpu_render_times),
            ("RenderTotal", total_render_times),
        ]

        test_case_results: list[trace_metrics.TestCaseResult] = []
        for name, values in metrics_list:
            if self.aggregates_only:
                test_case_results.extend(
                    trace_utils.standard_metrics_set(
                        values=values,
                        label_prefix=name,
                        unit=trace_metrics.Unit.milliseconds,
                    )
                )
            else:
                test_case_results.append(
                    trace_metrics.TestCaseResult(
                        name, trace_metrics.Unit.milliseconds, values
                    )
                )

        return test_case_results
