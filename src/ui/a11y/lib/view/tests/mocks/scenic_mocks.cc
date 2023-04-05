// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/a11y/lib/view/tests/mocks/scenic_mocks.h"

namespace accessibility_test {
namespace {

using fuchsia::ui::pointer::EventPhase;
using fuchsia::ui::pointer::TouchEvent;
using fuchsia::ui::pointer::TouchInteractionId;
using fuchsia::ui::pointer::TouchPointerSample;
using fuchsia::ui::pointer::TouchResponse;
using fuchsia::ui::pointer::TouchResponseType;
using fuchsia::ui::pointer::ViewParameters;
using fuchsia::ui::pointer::augment::TouchEventWithLocalHit;
using fuchsia::ui::pointer::augment::TouchSourceWithLocalHit;
using fuchsia::ui::pointer::augment::TouchSourceWithLocalHitPtr;

TouchEventWithLocalHit fake_touch_event(EventPhase phase, uint64_t view_ref_koid_for_hit = 0,
                                        uint32_t interaction_id = 0,
                                        std::array<float, 2> position_in_viewport = {0, 0}) {
  TouchPointerSample sample;
  sample.set_interaction({0, 0, interaction_id});
  sample.set_phase(phase);
  sample.set_position_in_viewport(position_in_viewport);

  TouchEvent inner;
  inner.set_timestamp(0);
  inner.set_pointer_sample(std::move(sample));
  inner.set_trace_flow_id(0);

  return {std::move(inner), view_ref_koid_for_hit, {0, 0}};
}

/*
std::vector<TouchEventWithLocalHit> n_events(uint64_t n) {
  std::vector<TouchEventWithLocalHit> events(n);
  for (uint32_t i = 0; i < n; ++i) {
    events[i] = fake_touch_event(EventPhase::CHANGE);
  }
  return events;
}
*/

TouchEventWithLocalHit fake_view_parameters() {
  const ViewParameters parameters = {
      .view = {{0, 0}, {1, 1}},
      .viewport = {{0, 0}, {1, 1}},
      .viewport_to_view_transform = {0},
  };

  TouchEvent inner;
  inner.set_view_parameters(parameters);

  return {
      .touch_event = std::move(inner),
      .local_viewref_koid = 0,
      .local_point = {0, 0},
  };
}

/*
bool interaction_equals(TouchInteractionId id1, TouchInteractionId id2) {
  return id1.device_id == id2.device_id && id1.pointer_id == id2.pointer_id &&
         id1.interaction_id == id2.interaction_id;
}
*/

}  // namespace

void MockSession::Enqueue(std::vector<fuchsia::ui::scenic::Command> cmds) {
  cmd_queue_.insert(cmd_queue_.end(), std::make_move_iterator(cmds.begin()),
                    std::make_move_iterator(cmds.end()));
}

void MockSession::Bind(fidl::InterfaceRequest<::fuchsia::ui::scenic::Session> request,
                       ::fuchsia::ui::scenic::SessionListenerPtr listener) {
  binding_.Bind(std::move(request));
  listener_ = std::move(listener);
  Reset();
}

void MockSession::Reset() {
  cmd_queue_.clear();
  view_holders_.clear();
  views_.clear();
  entity_nodes_.clear();
  rectangle_nodes_.clear();
  rectangles_.clear();
}

void MockSession::ApplyCreateResourceCommand(const fuchsia::ui::gfx::CreateResourceCmd& command) {
  const uint32_t id = command.id;
  switch (command.resource.Which()) {
    case fuchsia::ui::gfx::ResourceArgs::Tag::kView3:
      views_[id].id = id;
      fidl::Clone(command.resource.view3().view_ref, &views_[id].view_ref);
      break;

    case fuchsia::ui::gfx::ResourceArgs::Tag::kViewHolder:
      view_holders_[id].id = id;
      fidl::Clone(command.resource.view_holder().token, &view_holders_[id].view_holder_token);
      break;

    case fuchsia::ui::gfx::ResourceArgs::Tag::kEntityNode:
      entity_nodes_[id].id = id;
      break;

    case fuchsia::ui::gfx::ResourceArgs::Tag::kShapeNode:
      rectangle_nodes_[id].id = id;
      break;

    case fuchsia::ui::gfx::ResourceArgs::Tag::kMaterial:
      materials_.emplace(id);
      break;

    case fuchsia::ui::gfx::ResourceArgs::Tag::kRectangle:
      rectangles_[id].id = id;
      rectangles_[id].width = command.resource.rectangle().width.vector1();
      rectangles_[id].height = command.resource.rectangle().height.vector1();
      break;

    default:
      break;
  }
}

void MockSession::ApplyAddChildCommand(const fuchsia::ui::gfx::AddChildCmd& command) {
  const uint32_t parent_id = command.node_id;
  const uint32_t child_id = command.child_id;

  // Update parent's children. Only views and entity nodes will have children. Also, resource ids
  // are unique globally across all resource types, so only one of views_ and entity_nodes_ will
  // contain parent_id as a key.
  if (views_.find(parent_id) != views_.end()) {
    views_[parent_id].children.insert(child_id);
  } else if (entity_nodes_.find(parent_id) != entity_nodes_.end()) {
    entity_nodes_[parent_id].children.insert(child_id);
  }

  // Update child's parent. Only entity, shape, and view holder nodes will have parents.
  // Furthermore, ids are unique across all resources in the session.
  if (entity_nodes_.find(child_id) != entity_nodes_.end()) {
    entity_nodes_[child_id].parent_id = parent_id;
  } else if (rectangle_nodes_.find(child_id) != rectangle_nodes_.end()) {
    rectangle_nodes_[child_id].parent_id = parent_id;
  } else if (view_holders_.find(child_id) != view_holders_.end()) {
    view_holders_[child_id].parent_id = parent_id;
  }
}

void MockSession::ApplySetMaterialCommand(const fuchsia::ui::gfx::SetMaterialCmd& command) {
  rectangle_nodes_[command.node_id].material_id = command.material_id;
}

void MockSession::ApplySetShapeCommand(const fuchsia::ui::gfx::SetShapeCmd& command) {
  const uint32_t node_id = command.node_id;
  const uint32_t rectangle_id = command.shape_id;

  rectangle_nodes_[node_id].rectangle_id = rectangle_id;
  rectangles_[rectangle_id].parent_id = node_id;
}

void MockSession::ApplySetTranslationCommand(const fuchsia::ui::gfx::SetTranslationCmd& command) {
  // For accessiblity purposes, only entity nodes and rectangles will have
  // translations.
  if (entity_nodes_.find(command.id) != entity_nodes_.end()) {
    entity_nodes_[command.id].translation_vector[0] = command.value.value.x;
    entity_nodes_[command.id].translation_vector[1] = command.value.value.y;
    entity_nodes_[command.id].translation_vector[2] = command.value.value.z;
  } else {
    const uint32_t parent_id = command.id;
    const uint32_t rectangle_id = rectangle_nodes_[parent_id].rectangle_id;
    const auto& translation = command.value.value;
    rectangles_[rectangle_id].center_x = translation.x;
    rectangles_[rectangle_id].center_y = translation.y;
    rectangles_[rectangle_id].elevation = translation.z;
  }
}

void MockSession::ApplySetScaleCommand(const fuchsia::ui::gfx::SetScaleCmd& command) {
  if (entity_nodes_.find(command.id) != entity_nodes_.end()) {
    entity_nodes_[command.id].scale_vector[0] = command.value.value.x;
    entity_nodes_[command.id].scale_vector[1] = command.value.value.y;
    entity_nodes_[command.id].scale_vector[2] = command.value.value.z;
  }
}

void MockSession::ApplyDetachCommand(const fuchsia::ui::gfx::DetachCmd& command) {
  const uint32_t id = command.id;

  // The annotation view only ever detaches the content entity node from the view node.
  auto& entity_node = entity_nodes_[id];

  if (entity_node.parent_id != 0) {
    views_[entity_node.parent_id].children.erase(id);
  }

  entity_node.parent_id = 0u;
}

void MockSession::ApplySetViewPropertiesCommand(
    const fuchsia::ui::gfx::SetViewPropertiesCmd& command) {
  const uint32_t id = command.view_holder_id;

  if (view_holders_.find(id) != view_holders_.end()) {
    view_holders_[id].id = id;
    view_holders_[id].properties = command.properties;
  }
}

void MockSession::Present(uint64_t presentation_time, ::std::vector<::zx::event> acquire_fences,
                          ::std::vector<::zx::event> release_fences, PresentCallback callback) {
  for (const auto& command : cmd_queue_) {
    if (command.Which() != fuchsia::ui::scenic::Command::Tag::kGfx) {
      continue;
    }

    const auto& gfx_command = command.gfx();

    switch (gfx_command.Which()) {
      case fuchsia::ui::gfx::Command::Tag::kCreateResource:
        ApplyCreateResourceCommand(gfx_command.create_resource());
        break;

      case fuchsia::ui::gfx::Command::Tag::kAddChild:
        ApplyAddChildCommand(gfx_command.add_child());
        break;

      case fuchsia::ui::gfx::Command::Tag::kSetMaterial:
        ApplySetMaterialCommand(gfx_command.set_material());
        break;

      case fuchsia::ui::gfx::Command::Tag::kSetShape:
        ApplySetShapeCommand(gfx_command.set_shape());
        break;

      case fuchsia::ui::gfx::Command::Tag::kSetTranslation:
        ApplySetTranslationCommand(gfx_command.set_translation());
        break;

      case fuchsia::ui::gfx::Command::Tag::kSetScale:
        ApplySetScaleCommand(gfx_command.set_scale());
        break;

      case fuchsia::ui::gfx::Command::Tag::kDetach:
        ApplyDetachCommand(gfx_command.detach());
        break;

      case fuchsia::ui::gfx::Command::Tag::kSetViewProperties:
        ApplySetViewPropertiesCommand(gfx_command.set_view_properties());
        break;

      default:
        break;
    }
  }

  callback(fuchsia::images::PresentationInfo());
}

void MockSession::SendGfxEvent(fuchsia::ui::gfx::Event event) {
  fuchsia::ui::scenic::Event scenic_event;
  scenic_event.set_gfx(std::move(event));

  std::vector<fuchsia::ui::scenic::Event> events;
  events.emplace_back(std::move(scenic_event));

  listener_->OnScenicEvent(std::move(events));
}

void MockSession::SendViewPropertiesChangedEvent(uint32_t view_id,
                                                 fuchsia::ui::gfx::ViewProperties properties) {
  fuchsia::ui::gfx::ViewPropertiesChangedEvent view_properties_changed_event = {
      .view_id = view_id,
      .properties = properties,
  };
  fuchsia::ui::gfx::Event event;
  event.set_view_properties_changed(view_properties_changed_event);

  SendGfxEvent(std::move(event));
}

void MockSession::SendViewDetachedFromSceneEvent(uint32_t view_id) {
  FX_DCHECK(views_.count(view_id));
  fuchsia::ui::gfx::ViewDetachedFromSceneEvent view_detached_from_scene_event = {.view_id =
                                                                                     view_id};
  fuchsia::ui::gfx::Event event;
  event.set_view_detached_from_scene(view_detached_from_scene_event);

  SendGfxEvent(std::move(event));
}

void MockSession::SendViewAttachedToSceneEvent(uint32_t view_id) {
  FX_DCHECK(views_.count(view_id));
  fuchsia::ui::gfx::ViewAttachedToSceneEvent view_attached_to_scene_event = {
      .view_id = view_id, .properties = kDefaultViewProperties};
  fuchsia::ui::gfx::Event event;
  event.set_view_attached_to_scene(view_attached_to_scene_event);

  SendGfxEvent(std::move(event));
}

void MockSession::SendViewConnectedEvent(uint32_t view_holder_id) {
  FX_DCHECK(view_holders_.count(view_holder_id));
  fuchsia::ui::gfx::ViewConnectedEvent view_connected_event = {.view_holder_id = view_holder_id};
  fuchsia::ui::gfx::Event event;
  event.set_view_connected(view_connected_event);

  SendGfxEvent(std::move(event));
}

void MockSession::SendViewHolderDisconnectedEvent(uint32_t view_id) {
  FX_DCHECK(views_.count(view_id));
  fuchsia::ui::gfx::ViewHolderDisconnectedEvent view_holder_disconnected_event = {.view_id =
                                                                                      view_id};
  fuchsia::ui::gfx::Event event;
  event.set_view_holder_disconnected(view_holder_disconnected_event);

  SendGfxEvent(std::move(event));
}

void MockScenic::CreateSession(
    fidl::InterfaceRequest<fuchsia::ui::scenic::Session> session,
    fidl::InterfaceHandle<fuchsia::ui::scenic::SessionListener> listener) {
  mock_session_->Bind(std::move(session), listener.Bind());
  create_session_called_ = true;
}

void MockScenic::CreateSessionT(fuchsia::ui::scenic::SessionEndpoints endpoints,
                                CreateSessionTCallback callback) {
  mock_session_->Bind(std::move(*endpoints.mutable_session()),
                      endpoints.mutable_session_listener()->Bind());

  // This binding is necessary because a11y code will use the client-side of the channel to call
  // Upgrade in the protocol fuchsia::ui::pointer::augment::LocalHit. If the channel is not bound to
  // a server end, fidl reports an error when you try to pass a channel that was not bound over a
  // message.
  touch_source_binding_.Bind(std::move(*endpoints.mutable_touch_source()));
  create_session_called_ = true;
}

fidl::InterfaceRequestHandler<fuchsia::ui::scenic::Scenic> MockScenic::GetHandler(
    async_dispatcher_t* dispatcher) {
  return [this, dispatcher](fidl::InterfaceRequest<fuchsia::ui::scenic::Scenic> request) {
    bindings_.AddBinding(this, std::move(request), dispatcher);
  };
}

MockLocalHit::MockLocalHit() : touch_source_binding_(this) {}

void MockLocalHit::Watch(std::vector<TouchResponse> responses, WatchCallback callback) {
  ++num_watch_calls_;
  responses_ = std::move(responses);
  callback_ = std::move(callback);
}

void MockLocalHit::UpdateResponse(TouchInteractionId interaction, TouchResponse response,
                                  UpdateResponseCallback callback) {
  updated_responses_.emplace_back(std::make_pair(interaction, std::move(response)));
}

uint32_t MockLocalHit::NumWatchCalls() const { return num_watch_calls_; }

void MockLocalHit::SimulateEvents(std::vector<TouchEventWithLocalHit> events) {
  FX_CHECK(callback_);
  callback_(std::move(events));
  callback_ = nullptr;
}

std::vector<TouchResponse> MockLocalHit::TakeResponses() { return std::move(responses_); }

std::vector<std::pair<TouchInteractionId, TouchResponse>> MockLocalHit::TakeUpdatedResponses() {
  return std::move(updated_responses_);
}

void MockLocalHit::EnqueueTapToEvents() {
  if (!view_parameters_sent_) {
    enqueued_events_.emplace_back(fake_view_parameters());
    view_parameters_sent_ = true;
  }
  enqueued_events_.emplace_back(fake_touch_event(EventPhase::ADD, view_ref_koid_for_hit_));
  enqueued_events_.emplace_back(fake_touch_event(EventPhase::CHANGE, view_ref_koid_for_hit_));
  enqueued_events_.emplace_back(fake_touch_event(EventPhase::REMOVE, view_ref_koid_for_hit_));
}

void MockLocalHit::SimulateEnqueuedEvents() {
  SimulateEvents(std::move(enqueued_events_));
  enqueued_events_.clear();
}

void MockLocalHit::SetViewRefKoidForTouchEvents(uint64_t view_ref_koid) {
  view_ref_koid_for_hit_ = view_ref_koid;
}

}  // namespace accessibility_test
