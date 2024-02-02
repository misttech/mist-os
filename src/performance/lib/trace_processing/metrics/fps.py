#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FPS trace metrics."""

import logging
from typing import Any, Dict, Iterable, Iterator, List

import trace_processing.trace_metrics as trace_metrics
import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils


_LOGGER: logging.Logger = logging.getLogger("FPSMetricsProcessor")
_AGGREGATE_METRICS_ONLY: str = "aggregateMetricsOnly"
_EVENT_CATEGORY: str = "gfx"
_SCENIC_RENDER_EVENT_NAME: str = "RenderFrame"
_DISPLAY_VSYNC_EVENT_NAME: str = "Display::Controller::OnDisplayVsync"


def metrics_processor(
    model: trace_model.Model, extra_args: Dict[str, Any]
) -> List[trace_metrics.TestCaseResult]:
    """Computes frames per second sent to display from Scenic for the given trace.

    Args:
        model: The trace model to process.
        extra_args: Additional arguments to the processor.

    Returns:
        A list of computed metrics.
    """

    all_events: Iterator[trace_model.Event] = model.all_events()
    cpu_render_start_events: Iterable[
        trace_model.Event
    ] = trace_utils.filter_events(
        all_events,
        category=_EVENT_CATEGORY,
        name=_SCENIC_RENDER_EVENT_NAME,
        type=trace_model.DurationEvent,
    )

    vsync_events: List[trace_model.Event] = list(
        filter(
            lambda item: item is not None,
            map(
                lambda e: trace_utils.get_nearest_following_event(
                    e, _EVENT_CATEGORY, _DISPLAY_VSYNC_EVENT_NAME
                ),
                cpu_render_start_events,
            ),
        )
    )

    if len(vsync_events) < 2:
        _LOGGER.info(
            f"Less than two vsync events are present. Perhaps the trace duration"
            f"is too short to provide fps information"
        )
        return []

    fps_values: List[float] = []
    for i in range(len(vsync_events) - 1):
        fps_values.append(
            trace_time.TimeDelta.from_seconds(1)
            / (vsync_events[i + 1].start - vsync_events[i].start)
        )

    fps_mean: float = trace_utils.mean(fps_values)
    _LOGGER.info(f"Average FPS: {fps_mean}")

    test_case_results: List[trace_metrics.TestCaseResult] = []
    if extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True:
        for percentile in [5, 25, 50, 75, 95]:
            fps_percentile: float = trace_utils.percentile(
                fps_values, percentile
            )
            test_case_results.append(
                trace_metrics.TestCaseResult(
                    f"FpsP{percentile}",
                    trace_metrics.Unit.framesPerSecond,
                    [fps_percentile],
                )
            )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"FpsMin", trace_metrics.Unit.framesPerSecond, [min(fps_values)]
            )
        )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"FpsMax", trace_metrics.Unit.framesPerSecond, [max(fps_values)]
            )
        )
        test_case_results.append(
            trace_metrics.TestCaseResult(
                f"FpsAverage", trace_metrics.Unit.framesPerSecond, [fps_mean]
            )
        )
    else:
        result: trace_metrics.TestCaseResult = trace_metrics.TestCaseResult(
            "Fps", trace_metrics.Unit.framesPerSecond, fps_values
        )
        test_case_results.append(result)

    return test_case_results
