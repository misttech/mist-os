#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""FPS trace metrics."""

import logging
import statistics
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


# DEPRECATED: Use FpsMetricsProcessor instead.
# TODO(b/320778225): Remove once downstream callers are migrated.
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
    return FpsMetricsProcessor(
        aggregates_only=extra_args.get(_AGGREGATE_METRICS_ONLY, False) is True
    ).process_metrics(model)


class FpsMetricsProcessor(trace_metrics.MetricsProcessor):
    """Computes FPS (Frames-per-Second) metrics."""

    def __init__(self, aggregates_only: bool = True):
        """Constructor.

        Args:
            aggregates_only: When True, generates FpsMin, FpsMax, FpsAverage and FpsP* (percentiles).
                Otherwise generates Fps metric with all Fps values.
        """
        self.aggregates_only: bool = aggregates_only

    def process_metrics(
        self, model: trace_model.Model
    ) -> List[trace_metrics.TestCaseResult]:
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
        # This method looks for a possible race between trace event start in Scenic and magma.
        # We can safely skip these events. See https://fxbug.dev/322849857 for more details.
        vsync_events = vsync_events[
            trace_utils.find_valid_vsync_start_index(vsync_events) :
        ]

        if len(vsync_events) < 2:
            _LOGGER.info(
                f"Less than two vsync events are present. Perhaps the trace duration"
                f"is too short to provide fps information"
            )
            return []

        fps_values: List[float] = []
        for i in range(len(vsync_events) - 1):
            # Two renders may be squashed into one.
            if vsync_events[i + 1].start == vsync_events[i].start:
                continue
            fps_values.append(
                trace_time.TimeDelta.from_seconds(1)
                / (vsync_events[i + 1].start - vsync_events[i].start)
            )

        if len(fps_values) == 0:
            _LOGGER.info(f"Not enough valid vsyncs")
            return []

        fps_mean: float = statistics.mean(fps_values)
        _LOGGER.info(f"Average FPS: {fps_mean}")

        if self.aggregates_only:
            return trace_utils.standard_metrics_set(
                values=fps_values,
                label_prefix="Fps",
                unit=trace_metrics.Unit.framesPerSecond,
            )
        else:
            return [
                trace_metrics.TestCaseResult(
                    "Fps", trace_metrics.Unit.framesPerSecond, fps_values
                )
            ]
