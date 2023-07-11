// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/input/mouse_system.h"

#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/input/internal_pointer_event.h"
#include "src/ui/scenic/lib/input/mouse_source.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/math.h"

#include <glm/glm.hpp>

namespace scenic_impl::input {

MouseSystem::MouseSystem(sys::ComponentContext* context,
                         std::shared_ptr<const view_tree::Snapshot>& view_tree_snapshot,
                         HitTester& hit_tester, fit::function<void(zx_koid_t)> request_focus)
    : view_tree_snapshot_(view_tree_snapshot),
      hit_tester_(hit_tester),
      request_focus_(std::move(request_focus)) {}

void MouseSystem::RegisterMouseSource(
    fidl::InterfaceRequest<fuchsia::ui::pointer::MouseSource> mouse_source_request,
    zx_koid_t client_view_ref_koid) {
  const auto [it, success] = mouse_sources_.emplace(
      client_view_ref_koid,
      std::make_unique<MouseSource>(std::move(mouse_source_request),
                                    /*error_handler*/ [this, client_view_ref_koid] {
                                      mouse_sources_.erase(client_view_ref_koid);
                                    }));
  FX_DCHECK(success);
}

zx_koid_t MouseSystem::FindViewRefKoidOfRelatedChannel(
    const fidl::InterfaceHandle<fuchsia::ui::pointer::MouseSource>& original) const {
  const zx_koid_t related_koid = fsl::GetRelatedKoid(original.channel().get());
  const auto it = std::find_if(
      mouse_sources_.begin(), mouse_sources_.end(),
      [related_koid](const auto& kv) { return kv.second->channel_koid() == related_koid; });
  return it == mouse_sources_.end() ? ZX_KOID_INVALID : it->first;
}

void MouseSystem::SendEventToMouse(zx_koid_t receiver, const InternalMouseEvent& event,
                                   const StreamId stream_id, bool view_exit) {
  const auto it = mouse_sources_.find(receiver);
  if (it != mouse_sources_.end()) {
    if (view_exit) {
      // Bounding box and correct transform does not matter on view exit (since we don't send any
      // pointer samples), and we are likely working with a broken ViewTree, so skip them.
      it->second->UpdateStream(stream_id, event, {}, view_exit);
    } else {
      it->second->UpdateStream(
          stream_id, EventWithReceiverFromViewportTransform(event, receiver, *view_tree_snapshot_),
          view_tree_snapshot_->view_tree.at(receiver).bounding_box, view_exit);
    }
  }
}

void MouseSystem::InjectMouseEventExclusive(const InternalMouseEvent& event,
                                            const StreamId stream_id) {
  FX_DCHECK(view_tree_snapshot_->IsDescendant(event.target, event.context))
      << "Should never allow injection into broken scene graph";
  FX_DCHECK(current_exclusive_mouse_receivers_.count(stream_id) == 0 ||
            current_exclusive_mouse_receivers_.at(stream_id) == event.target);
  current_exclusive_mouse_receivers_[stream_id] = event.target;
  SendEventToMouse(event.target, event, stream_id, /*view_exit=*/false);
}

void MouseSystem::InjectMouseEventHitTested(const InternalMouseEvent& event,
                                            const StreamId stream_id) {
  FX_DCHECK(view_tree_snapshot_->IsDescendant(event.target, event.context))
      << "Should never allow injection into broken scene graph";
  // Grab the current mouse receiver or create a new one.
  MouseReceiver& mouse_receiver = current_mouse_receivers_[stream_id];

  // Unlatch a current latch if all buttons are released.
  const bool button_down = !event.buttons.pressed.empty();
  mouse_receiver.latched = mouse_receiver.latched && button_down;

  // If the scene graph breaks while latched -> send a "View Exited" event and invalidate the
  // receiver for the remainder of the latch.
  if (mouse_receiver.latched &&
      !view_tree_snapshot_->IsDescendant(mouse_receiver.view_koid, event.target) &&
      mouse_receiver.view_koid != event.target) {
    SendEventToMouse(mouse_receiver.view_koid, event, stream_id, /*view_exit=*/true);
    mouse_receiver.view_koid = ZX_KOID_INVALID;
    return;
  }
  // If not latched, choose the current target by finding the top view.
  if (!mouse_receiver.latched) {
    const zx_koid_t top_koid = hit_tester_.TopHitTest(event, /*semantic_hit_test*/ false);

    // Determine the currently hovered view. If it's different than previously, send the
    // previous one a "View Exited" event.
    if (mouse_receiver.view_koid != top_koid) {
      SendEventToMouse(mouse_receiver.view_koid, event, stream_id, /*view_exit=*/true);
    }
    mouse_receiver.view_koid = top_koid;

    // Button down on an unlatched stream -> latch it to the top-most view.
    if (button_down) {
      mouse_receiver.latched = true;
      request_focus_(mouse_receiver.view_koid);
    }
  }

  // Finally, send the event to the hovered/latched view.
  SendEventToMouse(mouse_receiver.view_koid, event, stream_id, /*view_exit=*/false);
}

void MouseSystem::CancelMouseStream(StreamId stream_id) {
  zx_koid_t receiver = ZX_KOID_INVALID;
  {
    const auto it = current_mouse_receivers_.find(stream_id);
    if (it != current_mouse_receivers_.end()) {
      receiver = it->second.view_koid;
      current_mouse_receivers_.erase(it);
    }
  }
  {
    const auto it = current_exclusive_mouse_receivers_.find(stream_id);
    if (it != current_exclusive_mouse_receivers_.end()) {
      receiver = it->second;
      current_exclusive_mouse_receivers_.erase(it);
    }
  }

  const auto it = mouse_sources_.find(receiver);
  if (it != mouse_sources_.end()) {
    it->second->UpdateStream(stream_id, {}, {}, /*view_exit=*/true);
  }
}

}  // namespace scenic_impl::input
