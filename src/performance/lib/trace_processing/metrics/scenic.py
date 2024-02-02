#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Scenic trace metrics."""

import logging
from typing import Any, Dict, Iterable, Iterator, List

import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils


_LOGGER: logging.Logger = logging.getLogger("ScenicMetricsProcessor")
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"
_EVENT_CATEGORY: str = "gfx"
_SCENIC_START_EVENT_NAME: str = "ApplyScheduledSessionUpdates"
_SCENIC_RENDER_EVENT_NAME: str = "RenderFrame"
_DISPLAY_VSYNC_READY_EVENT_NAME: str = "Display::Fence::OnReady"


def metrics_processor(
    model: trace_model.Model, extra_args: Dict[str, Any]
) -> List[trace_metrics.TestCaseResult]:
    """Computes how long Scenic takes for operations for the given trace.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """

    all_events: Iterator[trace_model.Event] = model.all_events()
    scenic_start_events: List[trace_model.Event] = list(
        trace_utils.filter_events(
            all_events,
            category=_EVENT_CATEGORY,
            name=_SCENIC_START_EVENT_NAME,
            type=trace_model.DurationEvent,
        )
    )
    scenic_render_events: List[trace_model.Event] = [
        trace_utils.get_nearest_following_event(
            e, _EVENT_CATEGORY, _SCENIC_RENDER_EVENT_NAME
        )
        for e in scenic_start_events
    ]
    vsync_ready_events: List[trace_model.Event] = [
        trace_utils.get_nearest_following_event(
            e, _EVENT_CATEGORY, _DISPLAY_VSYNC_READY_EVENT_NAME
        )
        for e in scenic_start_events
    ]

    if len(scenic_render_events) < 1 or len(vsync_ready_events) < 1:
        _LOGGER.info(
            f"No render or vsync events are present. Perhaps the trace duration"
            f"is too short to provide scenic render information"
        )
        return []

    cpu_render_times: List[float] = []
    for start_event, render_event in zip(
        scenic_start_events, scenic_render_events
    ):
        if render_event is None:
            continue
        cpu_render_times.append(
            (
                render_event.start + render_event.duration - start_event.start
            ).to_milliseconds_f()
        )

    cpu_render_mean: float = trace_utils.mean(cpu_render_times)
    _LOGGER.info(f"Average CPU render time: {cpu_render_mean} ms")

    total_render_times: List[float] = []
    for start_event, vsync_event in zip(
        scenic_start_events, vsync_ready_events
    ):
        if vsync_event is None:
            continue
        total_render_times.append(
            (vsync_event.start - start_event.start).to_milliseconds_f()
        )

    total_render_mean: float = trace_utils.mean(total_render_times)
    _LOGGER.info(f"Average Total render time: {total_render_mean} ms")

    metrics_list: List[Tuple[str, List[float]]] = [
        ("RenderCpu", cpu_render_times),
        ("RenderTotal", total_render_times),
    ]

    test_case_results: List[trace_metrics.TestCaseResult] = []
    for name, values in metrics_list:
        if extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True:
            for percentile in [5, 25, 50, 75, 95]:
                render_percentile: float = trace_utils.percentile(
                    values, percentile
                )
                test_case_results.append(
                    trace_metrics.TestCaseResult(
                        name + f"P{percentile}",
                        trace_metrics.Unit.milliseconds,
                        [render_percentile],
                    )
                )
            test_case_results.append(
                trace_metrics.TestCaseResult(
                    name + "Min",
                    trace_metrics.Unit.milliseconds,
                    [min(values)],
                )
            )
            test_case_results.append(
                trace_metrics.TestCaseResult(
                    name + "Max",
                    trace_metrics.Unit.milliseconds,
                    [max(values)],
                )
            )
            test_case_results.append(
                trace_metrics.TestCaseResult(
                    name + "Average",
                    trace_metrics.Unit.milliseconds,
                    [trace_utils.mean(values)],
                )
            )
        else:
            result: trace_metrics.TestCaseResult = trace_metrics.TestCaseResult(
                name, trace_metrics.Unit.milliseconds, values
            )
            test_case_results.append(result)

    return test_case_results
