// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scenic/util/print_event.h"

#include <lib/syslog/cpp/macros.h>

#if FUCHSIA_API_LEVEL_AT_MOST(17)

using fuchsia::ui::gfx::Event;
using fuchsia::ui::gfx::ImportUnboundEvent;
using fuchsia::ui::gfx::MetricsEvent;
using fuchsia::ui::gfx::ViewAttachedToSceneEvent;
using fuchsia::ui::gfx::ViewDetachedFromSceneEvent;
using fuchsia::ui::gfx::ViewDisconnectedEvent;
using fuchsia::ui::gfx::ViewPropertiesChangedEvent;

std::ostream& operator<<(std::ostream& stream, const fuchsia::ui::gfx::Event& event) {
  switch (event.Which()) {
    case Event::Tag::kMetrics:
      return stream << event.metrics();
    case Event::Tag::kSizeChangeHint:
      return stream << event.size_change_hint();
    case Event::Tag::kImportUnbound:
      return stream << event.import_unbound();
    case Event::Tag::kViewConnected:
      return stream << event.view_connected();
    case Event::Tag::kViewDisconnected:
      return stream << event.view_disconnected();
    case Event::Tag::kViewHolderConnected:
      return stream << event.view_holder_connected();
    case Event::Tag::kViewHolderDisconnected:
      return stream << event.view_holder_disconnected();
    case Event::Tag::kViewAttachedToScene:
      return stream << event.view_attached_to_scene();
    case Event::Tag::kViewDetachedFromScene:
      return stream << event.view_detached_from_scene();
    case Event::Tag::kViewPropertiesChanged:
      return stream << event.view_properties_changed();
    case Event::Tag::kViewStateChanged:
      return stream << event.view_state_changed();
    case Event::Tag::Invalid:
      return stream << "Invalid";
  }
}

std::ostream& operator<<(std::ostream& stream, const fuchsia::ui::gfx::MetricsEvent& event) {
  return stream << "MetricsEvent(node_id=" << event.node_id << ", metrics=<TBD>)";
}

std::ostream& operator<<(std::ostream& stream, const fuchsia::ui::gfx::SizeChangeHintEvent& event) {
  return stream << "SizeChangeHintEvent(node_id=" << event.node_id << ", size_change_factor=<TBD>)";
}

std::ostream& operator<<(std::ostream& stream, const fuchsia::ui::gfx::ImportUnboundEvent& event) {
  return stream << "ImportUnboundEvent(resource_id=" << event.resource_id << ")";
}

std::ostream& operator<<(std::ostream& stream, const fuchsia::ui::gfx::ViewConnectedEvent& event) {
  return stream << "ViewConnectedEvent(view_holder_id=" << event.view_holder_id << ")";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewDisconnectedEvent& event) {
  return stream << "ViewDisconnectedEvent(view_holder_id=" << event.view_holder_id << ")";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewHolderConnectedEvent& event) {
  return stream << "ViewHolderConnectedEvent(view_id=" << event.view_id << ")";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewHolderDisconnectedEvent& event) {
  return stream << "ViewHolderDisconnectedEvent(view_id=" << event.view_id << ")";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewAttachedToSceneEvent& event) {
  return stream << "ViewAttachedToSceneEvent(view_id=" << event.view_id << ", properties=<TBD>";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewDetachedFromSceneEvent& event) {
  return stream << "ViewDetachedFromSceneEvent(view_id=" << event.view_id << ")";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewPropertiesChangedEvent& event) {
  return stream << "ViewPropertiesChangedEvent(view_id=" << event.view_id << ", properties=<TBD>)";
}

std::ostream& operator<<(std::ostream& stream,
                         const fuchsia::ui::gfx::ViewStateChangedEvent& event) {
  return stream << "ViewStateChangedEvent(view_id=" << event.view_holder_id
                << ", state=.is_rendering" << event.state.is_rendering << ")";
}

#endif  // FUCHSIA_API_LEVEL_AT_MOST(17)
