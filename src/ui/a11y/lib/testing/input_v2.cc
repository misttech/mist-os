// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/a11y/lib/testing/input_v2.h"

#include "fuchsia/ui/pointer/augment/cpp/fidl.h"
#include "fuchsia/ui/pointer/cpp/fidl.h"

namespace accessibility_test::input_v2 {

namespace {

// Trivial conversion, just massaging types.
std::array<float, 2> PointFToPoint2(::fuchsia::math::PointF point) { return {point.x, point.y}; }

}  // namespace

using fuchsia::ui::pointer::EventPhase;

PointerParams::PointerParams(PointerId pointer_id, EventPhase phase, const glm::vec2& coordinate)
    : pointer_id(pointer_id), phase(phase), coordinate(coordinate) {}

std::vector<PointerParams> DownEvents(PointerId pointer_id, const glm::vec2& coordinate) {
  return {{pointer_id, EventPhase::ADD, coordinate}};
}

std::vector<PointerParams> UpEvents(PointerId pointer_id, const glm::vec2& coordinate) {
  return {{pointer_id, EventPhase::REMOVE, coordinate}};
}

std::vector<PointerParams> TapEvents(PointerId pointer_id, const glm::vec2& coordinate) {
  return DownEvents(pointer_id, coordinate) + UpEvents(pointer_id, coordinate);
}

// Pointer move events between two endpoints, (start, end]. The start point is exclusive and the end
// point is inclusive, as move events signify where a pointer has moved to rather than where it has
// moved from.
std::vector<PointerParams> MoveEvents(PointerId pointer_id, const glm::vec2& start,
                                      const glm::vec2& end, size_t moves) {
  std::vector<PointerParams> events;
  events.reserve(moves);
  for (size_t i = 1; i <= moves; ++i) {
    events.emplace_back(pointer_id, EventPhase::CHANGE,
                        start + (end - start) * static_cast<float>(i) / static_cast<float>(moves));
  }

  return events;
}

std::vector<PointerParams> DragEvents(PointerId pointer_id, const glm::vec2& start,
                                      const glm::vec2& end, size_t moves) {
  return DownEvents(pointer_id, start) + MoveEvents(pointer_id, start, end, moves) +
         UpEvents(pointer_id, end);
}

fuchsia::ui::pointer::augment::TouchEventWithLocalHit ToTouchEvent(const PointerParams& params,
                                                                   uint64_t event_time,
                                                                   zx_koid_t koid) {
  fuchsia::ui::pointer::TouchInteractionId interaction = {
      .device_id = 1,
      .pointer_id = params.pointer_id,
      .interaction_id = 0,
  };

  fuchsia::ui::pointer::TouchPointerSample sample;
  sample.set_interaction(interaction);
  sample.set_phase(params.phase);
  sample.set_position_in_viewport({params.coordinate.x, params.coordinate.y});

  fuchsia::ui::pointer::TouchEvent inner;
  inner.set_timestamp(static_cast<int64_t>(event_time));
  inner.set_pointer_sample(std::move(sample));
  static uint64_t trace_flow_id = 0;
  inner.set_trace_flow_id(trace_flow_id++);

  return {
      .touch_event = std::move(inner),
      .local_viewref_koid = koid,
      .local_point = PointFToPoint2(ToLocalCoordinates(params.coordinate)),
  };
}

::fuchsia::math::PointF ToLocalCoordinates(const glm::vec2& ndc) {
  return {ndc.x * kTestNDCToLocalMultiplier, ndc.y * kTestNDCToLocalMultiplier};
}

}  // namespace accessibility_test::input_v2
