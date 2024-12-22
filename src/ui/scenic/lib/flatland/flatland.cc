// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/flatland/flatland.h"

#include <fidl/fuchsia.math/cpp/fidl.h>
#include <fidl/fuchsia.ui.composition/cpp/natural_ostream.h>
#include <fidl/fuchsia.ui.composition/cpp/natural_types.h>
#include <lib/async/default.h>
#include <lib/async/time.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/ui/scenic/cpp/view_identity.h>
#include <lib/zx/eventpair.h>
#include <limits.h>
#include <zircon/errors.h>

#include <cstdint>
#include <functional>
#include <memory>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/allocation/id.h"
#include "src/ui/scenic/lib/flatland/flatland_types.h"
#include "src/ui/scenic/lib/scheduling/id.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/logging.h"
#include "src/ui/scenic/lib/utils/validate_eventpair.h"

#include <glm/gtc/constants.hpp>
#include <glm/gtc/matrix_access.hpp>
#include <glm/gtc/type_ptr.hpp>

using fuchsia_math::RectF;
using fuchsia_math::SizeU;
using fuchsia_math::Vec;
using fuchsia_math::VecF;
using fuchsia_ui_composition::ChildViewStatus;
using fuchsia_ui_composition::ChildViewWatcher;
using fuchsia_ui_composition::FlatlandError;
using fuchsia_ui_composition::HitRegion;
using fuchsia_ui_composition::ImageProperties;
using fuchsia_ui_composition::OnNextFrameBeginValues;
using fuchsia_ui_composition::Orientation;
using fuchsia_ui_composition::ParentViewportWatcher;
using fuchsia_ui_composition::ViewportProperties;
using fuchsia_ui_views::ViewCreationToken;
using fuchsia_ui_views::ViewportCreationToken;

namespace {

// Handle floating point errors up to an epsilon for sample region calls.
void ClampIfNear(float* val, float difference) {
  if (difference > 0.f && difference < 1e-3f) {
    *val -= difference;
  }
}

std::optional<std::string> ValidateViewportProperties(const ViewportProperties& properties) {
  if (properties.logical_size().has_value()) {
    const auto& logical_size = properties.logical_size();
    if (logical_size->width() == 0 || logical_size->height() == 0) {
      std::ostringstream stream;
      stream << "Logical_size components must be positive, given (" << logical_size->width() << ", "
             << logical_size->height() << ")";
      return stream.str();
    }
  }

  if (properties.inset().has_value()) {
    const auto inset = properties.inset();
    if (inset->top() < 0 || inset->right() < 0 || inset->bottom() < 0 || inset->left() < 0) {
      std::ostringstream stream;
      stream << "Inset components must be >= 0, given (" << inset->top() << ", " << inset->right()
             << ", " << inset->bottom() << ", " << inset->left() << ")";
      return stream.str();
    }
  }

  return std::nullopt;
}

void SetViewportPropertiesMissingDefaults(ViewportProperties& properties,
                                          const fuchsia_math::SizeU& logical_size,
                                          const fuchsia_math::Inset& inset) {
  if (!properties.logical_size().has_value()) {
    properties.logical_size(logical_size);
  }
  if (!properties.inset().has_value()) {
    properties.inset(inset);
  }
}

}  // namespace

namespace flatland {

std::shared_ptr<Flatland> Flatland::New(
    std::shared_ptr<utils::DispatcherHolder> dispatcher_holder,
    fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end, scheduling::SessionId session_id,
    std::function<void()> destroy_instance_function,
    std::shared_ptr<FlatlandPresenter> flatland_presenter, std::shared_ptr<LinkSystem> link_system,
    std::shared_ptr<UberStructSystem::UberStructQueue> uber_struct_queue,
    const std::vector<std::shared_ptr<allocation::BufferCollectionImporter>>&
        buffer_collection_importers,
    fit::function<void(fidl::ServerEnd<fuchsia_ui_views::Focuser>, zx_koid_t)>
        register_view_focuser,
    fit::function<void(fidl::ServerEnd<fuchsia_ui_views::ViewRefFocused>, zx_koid_t)>
        register_view_ref_focused,
    fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::TouchSource>, zx_koid_t)>
        register_touch_source,
    fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::MouseSource>, zx_koid_t)>
        register_mouse_source) {
  // clang-format off
  auto flatland = std::shared_ptr<Flatland>(new Flatland(
      dispatcher_holder,
      session_id,
      std::move(flatland_presenter),
      std::move(link_system),
      std::move(uber_struct_queue),
      buffer_collection_importers,
      std::move(register_view_focuser),
      std::move(register_view_ref_focused),
      std::move(register_touch_source),
      std::move(register_mouse_source)));
  // clang-format on

  // Natural FIDL bindings must be created and deleted on the same thread that it handles messages.
  async::PostTask(dispatcher_holder->dispatcher(),
                  [flatland, server_end = std::move(server_end),
                   destroy_instance_function = std::move(destroy_instance_function)]() mutable {
                    flatland->Bind(std::move(server_end), std::move(destroy_instance_function));
                  });

  return flatland;
}

Flatland::Flatland(std::shared_ptr<utils::DispatcherHolder> dispatcher_holder,
                   scheduling::SessionId session_id,
                   std::shared_ptr<FlatlandPresenter> flatland_presenter,
                   std::shared_ptr<LinkSystem> link_system,
                   std::shared_ptr<UberStructSystem::UberStructQueue> uber_struct_queue,
                   const std::vector<std::shared_ptr<allocation::BufferCollectionImporter>>&
                       buffer_collection_importers,
                   fit::function<void(fidl::ServerEnd<fuchsia_ui_views::Focuser>, zx_koid_t)>
                       register_view_focuser,
                   fit::function<void(fidl::ServerEnd<fuchsia_ui_views::ViewRefFocused>, zx_koid_t)>
                       register_view_ref_focused,
                   fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::TouchSource>, zx_koid_t)>
                       register_touch_source,
                   fit::function<void(fidl::ServerEnd<fuchsia_ui_pointer::MouseSource>, zx_koid_t)>
                       register_mouse_source)
    : dispatcher_holder_(std::move(dispatcher_holder)),
      session_id_(session_id),
      present2_helper_([this](fuchsia_scenic_scheduling::FramePresentedInfo info) {
        // If this callback is invoked, we know that `Present()` must have been called, and
        // therefore also know that binding must have been completed, because otherwise `Present()`
        // wouldn't have been called.
        //
        // Caveat: in Flatland unit tests, we invoke methods directly on the Flatland object, not
        // via a FIDL client.  It is conceivable that flakes might arise if the timing relationship
        // with the scheduler changes.
        if (this->binding_data_) {
          this->binding_data_->SendOnFramePresented(std::move(info));
        }
      }),
      flatland_presenter_(std::move(flatland_presenter)),
      link_system_(std::move(link_system)),
      uber_struct_queue_(std::move(uber_struct_queue)),
      buffer_collection_importers_(buffer_collection_importers),
      transform_graph_(session_id_),
      local_root_(transform_graph_.CreateTransform()),
      error_reporter_(scenic_impl::ErrorReporter::DefaultUnique()),
      images_to_release_(std::make_shared<std::unordered_set<allocation::GlobalImageId>>()),
      register_view_focuser_(std::move(register_view_focuser)),
      register_view_ref_focused_(std::move(register_view_ref_focused)),
      register_touch_source_(std::move(register_touch_source)),
      register_mouse_source_(std::move(register_mouse_source)) {
  FX_DCHECK(flatland_presenter_);

  FLATLAND_VERBOSE_LOG << "Flatland new with ID: " << session_id_;
}

void Flatland::Bind(fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end,
                    std::function<void()> destroy_instance_function) {
  // Only called once, by the constructor.
  FX_DCHECK(!binding_data_);
  binding_data_ =
      std::make_unique<BindingData>(this, dispatcher_holder_->dispatcher(), std::move(server_end),
                                    std::move(destroy_instance_function));

  FLATLAND_VERBOSE_LOG << "Flatland with ID: " << session_id_ << "  bound to FIDL channel.";
}

Flatland::BindingData::BindingData(Flatland* flatland, async_dispatcher_t* dispatcher,
                                   fidl::ServerEnd<fuchsia_ui_composition::Flatland> server_end,
                                   std::function<void()> destroy_instance_function)
    : binding_(dispatcher, std::move(server_end), flatland, std::mem_fn(&Flatland::OnFidlClosed)),
      destroy_instance_function_(std::move(destroy_instance_function)) {}

Flatland::BindingData::~BindingData() { destroy_instance_function_(); }

void Flatland::BindingData::SendOnFramePresented(
    fuchsia_scenic_scheduling::FramePresentedInfo info) {
  auto result = fidl::SendEvent(binding_)->OnFramePresented(
      fuchsia_ui_composition::FlatlandOnFramePresentedRequest(std::move(info)));
  if (result.is_error()) {
    auto& error = result.error_value().error();
    FX_LOGS(WARNING) << "SendOnFramePresented(): error while sending FIDL event: " << error.status()
                     << " " << error.status_string();
  }
}

void Flatland::BindingData::SendOnNextFrameBegin(uint32_t additional_present_credits,
                                                 FuturePresentationInfos presentation_infos) {
  OnNextFrameBeginValues values;
  values.additional_present_credits(additional_present_credits);
  values.future_presentation_infos(std::move(presentation_infos));

  auto result = fidl::SendEvent(binding_)->OnNextFrameBegin(
      fuchsia_ui_composition::FlatlandOnNextFrameBeginRequest(std::move(values)));
  if (result.is_error()) {
    auto& error = result.error_value().error();
    FX_LOGS(WARNING) << "SendOnNextFrameBegin(): error while sending FIDL event: " << error.status()
                     << " " << error.status_string();
  }
}

void Flatland::BindingData::CloseConnection(FlatlandError error) {
  // NOTE: there's no need to test the return values of OnError()/Cancel()/Close().  If they fail,
  // the binding and waiter will be cleaned up anyway because we'll soon be destroyed (since
  // destroy_instance_function_ has been or will be invoked).

  // Send the error to the client before closing the connection.
  auto result = fidl::SendEvent(binding_)->OnError(error);
  if (result.is_error()) {
    auto& error = result.error_value().error();
    FX_LOGS(WARNING) << "CloseConnection(): error while sending FIDL event: " << error.status()
                     << " " << error.status_string();
  }

  // Immediately close the FIDL interface to prevent future requests.
  binding_.Close(ZX_ERR_BAD_STATE);
}

Flatland::~Flatland() {
  // TODO(https://fxbug.dev/42132996): consider if Link tokens should be returned or not.

  // Clear the scene graph, then collect the images to release.
  Clear();
  auto data = transform_graph_.ComputeAndCleanup(GetRoot(), std::numeric_limits<uint64_t>::max());

  // We don't care about the images returned by `ProcessDeadTransforms()` because we want to release
  // all the images in `images_to_release_`, which potentially includes some added by
  // `ProcessDeadTransforms()`.
  ProcessDeadTransforms(data);
  FX_DCHECK(image_metadatas_.empty());

  // If there are any images to release, set up a waiter, and pass the event-to-be-signaled to
  // `FlatlandPresenter::RemoveSession`.  This will schedule another frame and signal the event
  // just like any other release fence.
  std::optional<zx::event> image_release_fence;
  if (!images_to_release_->empty()) {
    zx::event evt = utils::CreateEvent();
    image_release_fence = utils::CopyEvent(evt);

    auto wait = std::make_shared<async::WaitOnce>(evt.get(), ZX_EVENT_SIGNALED);
    zx_status_t status = wait->Begin(
        dispatcher(),
        [importer_refs = buffer_collection_importers_, images_to_release = images_to_release_,
         // We keep several objects alive in the closure:
         //   - the dispatcher, which is about to be released by the Flatland and FlatlandManager.
         //   - the wait object keeps itself alive via the ref in this closure
         //   - the waited-upon fence event: we retain a copy of the handle to avoid reasoning about
         //     whether the FlatlandPresenter implementation will safely keep it alive.
         keepalive_dispatcher = dispatcher_holder_, keepalive_wait = wait,
         keepalive_evt = std::move(evt)](async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                                         const zx_packet_signal_t* /*signal*/) mutable {
          for (auto& image_id : *images_to_release) {
            for (auto& importer : importer_refs) {
              importer->ReleaseBufferImage(image_id);
            }
          }
          images_to_release->clear();
        });
    FX_DCHECK(status == ZX_OK);
  }

  // This will signal the release fence (if any) that we pass to it, and therefore enable the wait
  // above to succeed.
  flatland_presenter_->RemoveSession(session_id_, std::move(image_release_fence));

  FLATLAND_VERBOSE_LOG << "Flatland destructor ran for ID: " << session_id_;
}

void Flatland::Present(PresentRequest& request, PresentCompleter::Sync& completer) {
  Present(std::move(request.args()));
}

void Flatland::Present(fuchsia_ui_composition::PresentArgs args) {
  // In Flatland unit tests, we invoke methods directly on this object, rather than using a FIDL
  // client over a Zircon channel.  In production situations, the channel is torn down at or before
  // the time that `binding_data_` is destroyed, and therefore there will be no subsequent method
  // invocations, including of `Present()`.
  if (!binding_data_) {
    FX_LOGS(WARNING)
        << "Ignoring Flatland::Present() called after binding_data_ was destroyed in session: "
        << session_id_ << "\nThis should not occur outside of unit tests.";
    return;
  }

  TRACE_DURATION("gfx", "Flatland::Present", "debug_name", TA_STRING(debug_name_.c_str()));
  std::string per_app_tracing_name = "Flatland::PerAppPresent[" + debug_name_ + "]";
  TRACE_DURATION("gfx", per_app_tracing_name.c_str());
  TRACE_FLOW_END("gfx", per_app_tracing_name.c_str(), present_count_);

  ++present_count_;

  FLATLAND_VERBOSE_LOG << "Flatland::Present() #" << present_count_ << " for " << local_root_ << " "
                       << this;

  // Close any clients that had invalid operations on link protocols.
  if (link_protocol_error_) {
    error_reporter_->ERROR() << "Link protocol error";
    CloseConnection(FlatlandError::kBadHangingGet);
    return;
  }

  // Close any clients that call Present() without any present tokens.
  if (present_credits_ == 0) {
    error_reporter_->ERROR() << "Out of present credits";
    CloseConnection(FlatlandError::kNoPresentsRemaining);
    return;
  }
  present_credits_--;

  // If any fields are missing, replace them with the default values.
  if (!args.requested_presentation_time().has_value()) {
    args.requested_presentation_time(0);
  }
  if (!args.release_fences().has_value()) {
    args.release_fences(std::vector<zx::event>{});
  }
  if (!args.acquire_fences().has_value()) {
    args.acquire_fences(std::vector<zx::event>{});
  }
  if (!args.unsquashable().has_value()) {
    args.unsquashable(false);
  }

  auto root_handle = GetRoot();

  // TODO(https://fxbug.dev/42116832): Decide on a proper limit on compute time for topological
  // sorting.
  auto data = transform_graph_.ComputeAndCleanup(root_handle, std::numeric_limits<uint64_t>::max());
  FX_DCHECK(data.iterations != std::numeric_limits<uint64_t>::max());

  // Don't commit changes if a cycle is detected. Instead, kill the channel and remove the sub-graph
  // from the global graph (the latter is the responsibility of the manager that is notified by
  // `BindingData::destroy_instance_function_`).
  if (!data.cyclical_edges.empty()) {
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FX_DCHECK(data.sorted_transforms[0].handle == root_handle);

  // Cleanup released resources. Here we also collect the list of unused images so they can be
  // released by the buffer collection importers.
  auto images_to_release = ProcessDeadTransforms(data);

  // If there are images ready for release, create a release fence for the current Present() and
  // delay release until that fence is reached to ensure that the images are no longer referenced
  // in any render data.
  if (!images_to_release.empty()) {
    // Create a release fence specifically for the images.
    zx::event image_release_fence;
    zx_status_t status = zx::event::create(0, &image_release_fence);
    FX_DCHECK(status == ZX_OK);

    // Use a self-referencing async::WaitOnce to perform ImageImporter deregistration.
    // This is primarily so the handler does not have to live in the Flatland instance, which may
    // be destroyed before the release fence is signaled.  `WaitOnce` moves the handler to the stack
    // prior to invoking it, so it is safe for the handler to delete the WaitOnce on exit.
    // Specifically, we move the wait object into the lambda function via |copy_ref = wait| to
    // ensure that the wait object lives. The callback will not trigger without this.
    auto wait = std::make_shared<async::WaitOnce>(image_release_fence.get(), ZX_EVENT_SIGNALED);
    status = wait->Begin(
        dispatcher(),
        [copy_ref = wait, importer_refs = buffer_collection_importers_, images_to_release,
         all_images_to_release = images_to_release_,
         session_id = session_id_](async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                                   const zx_packet_signal_t* /*signal*/) mutable {
          // The wait is canceled if the dispatcher is destroyed before the event is signaled.
          // In this case, we expect the images to have already been released by the wait in the
          // ~Flatland() destructor.
          FX_DCHECK(status == ZX_OK || status == ZX_ERR_CANCELED)
              << "status is: " << zx_status_get_string(status) << " (" << status << ")";
          if (status == ZX_ERR_CANCELED) {
            FX_DCHECK(all_images_to_release->empty());
            return;
          }

          for (auto& image_id : images_to_release) {
            if (!all_images_to_release->erase(image_id)) {
              // This is harmless, but typically shouldn't happen.  The rare exception is a race
              // when the Flatland session is being torn down, if this runs after the session is
              // destroyed, but before the session's loop/thread is stopped.
              FX_LOGS(WARNING) << "Flatland session << " << session_id
                               << " did not find expected image " << image_id
                               << " in images_to_release_";
              continue;
            }

            for (auto& importer : importer_refs) {
              importer->ReleaseBufferImage(image_id);
            }
          }
        });
    FX_DCHECK(status == ZX_OK) << "status is: " << status;

    // Push the new release fence into the user-provided list.
    args.release_fences()->push_back(std::move(image_release_fence));
  }

  auto uber_struct = std::make_unique<UberStruct>();
  uber_struct->local_topology = std::move(data.sorted_transforms);

  for (const auto& [handle, matrix_data] : matrices_) {
    uber_struct->local_matrices[handle] = matrix_data.GetMatrix();
  }

  for (const auto& [handle, sample_region] : image_sample_regions_) {
    uber_struct->local_image_sample_regions[handle] = sample_region;
  }

  for (const auto& [handle, opacity_value] : opacity_values_) {
    uber_struct->local_opacity_values[handle] = opacity_value;
  }

  for (const auto& [handle, clip_region] : clip_regions_) {
    uber_struct->local_clip_regions[handle] = clip_region;
  }

  for (const auto& [handle, hit_regions] : hit_regions_) {
    uber_struct->local_hit_regions_map[handle] = hit_regions;
  }

  // As per the default hit region policy, if the client has not explicitly set a hit region on the
  // root, add a full screen one.
  if (root_transform_.GetInstanceId() != 0 &&
      hit_regions_.find(root_transform_) == hit_regions_.end()) {
    uber_struct->local_hit_regions_map[root_transform_] = {{flatland::HitRegion::Infinite()}};
  }

  uber_struct->images = image_metadatas_;

  if (link_to_parent_.has_value()) {
    uber_struct->view_ref = link_to_parent_->view_ref;
  }

  uber_struct->debug_name = debug_name_;

  // Obtain the PresentId which is needed to:
  // - enqueue the UberStruct.
  // - schedule a frame
  // - notify client when the frame has been presented
  auto present_id = scheduling::GetNextPresentId();
  present2_helper_.RegisterPresent(present_id,
                                   /*present_received_time=*/zx::time(async_now(dispatcher())));

  TRACE_FLOW_BEGIN("gfx", "ScheduleUpdate", present_id);
  TRACE_FLOW_BEGIN("gfx", "wait_for_fences", SESSION_TRACE_ID(session_id_, present_id));

  // Safe to capture |this| because the Flatland is guaranteed to outlive |fence_queue_|,
  // Flatland is non-movable and FenceQueue does not fire closures after destruction.
  // TODO(https://fxbug.dev/42156567): make the fences be the first arg, and the closure be the
  // second.
  fence_queue_->QueueTask(
      [this, present_id, requested_presentation_time = args.requested_presentation_time().value(),
       unsquashable = args.unsquashable().value(), uber_struct = std::move(uber_struct),
       link_operations = std::move(pending_link_operations_),
       release_fences = std::move(*args.release_fences())]() mutable {
        // NOTE: this name is important for benchmarking.  Do not remove or modify it
        // without also updating the "process_gfx_trace.go" script.
        TRACE_DURATION("gfx", "scenic_impl::Session::ScheduleNextPresent", "session_id",
                       session_id_, "requested_presentation_time", requested_presentation_time);
        TRACE_FLOW_END("gfx", "wait_for_fences", SESSION_TRACE_ID(session_id_, present_id));

        // Push the UberStruct, then schedule the associated Present that will eventually publish
        // it to the InstanceMap used for rendering.
        uber_struct_queue_->Push(present_id, std::move(uber_struct));
        flatland_presenter_->ScheduleUpdateForSession(zx::time(requested_presentation_time),
                                                      {session_id_, present_id}, unsquashable,
                                                      std::move(release_fences));

        // Finalize Link destruction operations after publishing the new UberStruct. This
        // ensures that any local Transforms referenced by the to-be-deleted Links are already
        // removed from the now-published UberStruct.
        for (auto& operation : link_operations) {
          operation();
        }
      },
      std::move(*args.acquire_fences()));
  pending_link_operations_.clear();
}

void Flatland::CreateView(CreateViewRequest& request, CreateViewCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Flatland::CreateView", "debug_name", TA_STRING(debug_name_.c_str()));
  CreateView(std::move(request.token()), std::move(request.parent_viewport_watcher()));
}

void Flatland::CreateView(
    fuchsia_ui_views::ViewCreationToken token,
    fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher) {
  CreateViewHelper(std::move(token), std::move(parent_viewport_watcher), std::nullopt,
                   std::nullopt);
}

void Flatland::CreateView2(CreateView2Request& request, CreateView2Completer::Sync& completer) {
  TRACE_DURATION("gfx", "Flatland::CreateView2", "debug_name", TA_STRING(debug_name_.c_str()));
  CreateView2(std::move(request.token()), std::move(request.view_identity()),
              std::move(request.protocols()), std::move(request.parent_viewport_watcher()));
}

void Flatland::CreateView2(
    fuchsia_ui_views::ViewCreationToken token,
    fuchsia_ui_views::ViewIdentityOnCreation view_identity,
    fuchsia_ui_composition::ViewBoundProtocols protocols,
    fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher) {
  CreateViewHelper(std::move(token), std::move(parent_viewport_watcher), std::move(view_identity),
                   std::move(protocols));
}

void Flatland::CreateViewHelper(
    fuchsia_ui_views::ViewCreationToken token,
    fidl::ServerEnd<fuchsia_ui_composition::ParentViewportWatcher> parent_viewport_watcher,
    std::optional<fuchsia_ui_views::ViewIdentityOnCreation> view_identity,
    std::optional<fuchsia_ui_composition::ViewBoundProtocols> protocols) {
  // Attempting to link with an invalid token will never succeed, so its better to fail early and
  // immediately close the link connection.
  if (!token.value().is_valid()) {
    error_reporter_->ERROR() << "CreateView failed, ViewCreationToken was invalid";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (view_identity.has_value() &&
      !utils::validate_viewref(view_identity->view_ref_control(), view_identity->view_ref())) {
    error_reporter_->ERROR() << "CreateView failed, ViewIdentityOnCreation was invalid";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FX_DCHECK(link_system_);

  if (protocols.has_value()) {
    FX_DCHECK(view_identity.has_value()) << "required for view-bound protocols";
    RegisterViewBoundProtocols(std::move(*protocols),
                               utils::ExtractKoid(view_identity->view_ref()));
  }
  // This portion of the method is not feed forward. This makes it possible for clients to receive
  // layout information before this operation has been presented. By initializing the link
  // immediately, parents can inform children of layout changes, and child clients can perform
  // layout decisions before their first call to Present().
  auto child_transform_handle = transform_graph_.CreateTransform();

  LinkSystem::LinkToParent new_link_to_parent = link_system_->CreateLinkToParent(
      dispatcher_holder_, fidl::NaturalToHLCPP(token),
      view_identity.has_value() ? std::optional(fidl::NaturalToHLCPP(*view_identity))
                                : std::nullopt,
      fidl::NaturalToHLCPP(parent_viewport_watcher), child_transform_handle,
      [ref = weak_from_this(), weak_dispatcher_holder = std::weak_ptr<utils::DispatcherHolder>(
                                   dispatcher_holder_)](const std::string& error_log) {
        if (auto dispatcher_holder = weak_dispatcher_holder.lock()) {
          FX_CHECK(dispatcher_holder->dispatcher() == async_get_default_dispatcher())
              << "Link protocol error reported on the wrong dispatcher.";
        }
        if (auto impl = ref.lock())
          impl->ReportLinkProtocolError(error_log);
      });

  FLATLAND_VERBOSE_LOG << "Flatland::CreateView() link-attachment-point: "
                       << child_transform_handle;

  // This portion of the method is feed-forward. The parent-child relationship between
  // |child_transform_handle| and |local_root_| establishes the Transform hierarchy between the two
  // instances, but the operation will not be visible until the next Present() call includes that
  // topology.
  if (link_to_parent_.has_value()) {
    bool child_removed =
        transform_graph_.RemoveChild(link_to_parent_->child_transform_handle, local_root_);
    FX_DCHECK(child_removed);

    bool transform_released =
        transform_graph_.ReleaseTransform(link_to_parent_->child_transform_handle);
    FX_DCHECK(transform_released);

    // Delay the destruction of the previous parent link until the next Present().
    pending_link_operations_.push_back([old_link_to_parent = std::move(link_to_parent_)]() mutable {
      old_link_to_parent.reset();
    });
  }

  {
    const bool child_added =
        transform_graph_.AddChild(new_link_to_parent.child_transform_handle, local_root_);
    FX_DCHECK(child_added);
  }
  link_to_parent_ = std::move(new_link_to_parent);
}

void Flatland::RegisterViewBoundProtocols(fuchsia_ui_composition::ViewBoundProtocols protocols,
                                          const zx_koid_t view_ref_koid) {
  FX_DCHECK(register_view_focuser_);
  FX_DCHECK(register_view_ref_focused_);
  FX_DCHECK(register_touch_source_);
  FX_DCHECK(register_mouse_source_);

  if (protocols.view_focuser().has_value()) {
    register_view_focuser_(std::move(*protocols.view_focuser()), view_ref_koid);
  }

  if (protocols.view_ref_focused().has_value()) {
    register_view_ref_focused_(std::move(*protocols.view_ref_focused()), view_ref_koid);
  }

  if (protocols.touch_source().has_value()) {
    register_touch_source_(std::move(*protocols.touch_source()), view_ref_koid);
  }

  if (protocols.mouse_source().has_value()) {
    register_mouse_source_(std::move(*protocols.mouse_source()), view_ref_koid);
  }
}

void Flatland::ReleaseView(ReleaseViewCompleter::Sync& completer) { ReleaseView(); }

void Flatland::ReleaseView() {
  if (!link_to_parent_) {
    error_reporter_->ERROR() << "ReleaseView failed, no existing parent Link";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // Deleting the old LinkToParent's Transform effectively changes this intance's root back to
  // |local_root_|.
  bool child_removed =
      transform_graph_.RemoveChild(link_to_parent_->child_transform_handle, local_root_);
  FX_DCHECK(child_removed);

  bool transform_released =
      transform_graph_.ReleaseTransform(link_to_parent_->child_transform_handle);
  FX_DCHECK(transform_released);

  // Move the old parent link into the delayed operation so that it isn't taken into account when
  // computing the local topology, but doesn't get deleted until after the new UberStruct is
  // published.
  auto old_link_to_parent = std::move(link_to_parent_.value());
  link_to_parent_.reset();

  // Delay the actual destruction of the Link until the next Present().
  pending_link_operations_.push_back([old_link_to_parent = std::move(old_link_to_parent)]() {});
}

void Flatland::Clear(ClearCompleter::Sync& completer) { Clear(); }

void Flatland::Clear() {
  // Clear user-defined mappings and local matrices.
  transforms_.clear();
  content_handles_.clear();
  matrices_.clear();

  // We always preserve the link origin when clearing the graph. This call will place all other
  // TransformHandles in the dead_transforms set in the next Present(), which will trigger cleanup
  // of Images and BufferCollections.
  transform_graph_.ResetGraph(local_root_);

  // If a parent Link exists, delay its destruction until Present().
  if (link_to_parent_.has_value()) {
    auto local_link = std::move(link_to_parent_);
    link_to_parent_.reset();

    pending_link_operations_.push_back(
        [local_link = std::move(local_link)]() mutable { local_link.reset(); });
  }

  // Delay destruction of all child Links until Present().
  auto local_links = std::move(links_to_children_);
  links_to_children_.clear();

  pending_link_operations_.push_back(
      [local_links = std::move(local_links)]() mutable { local_links.clear(); });

  debug_name_.clear();
}

void Flatland::CreateTransform(CreateTransformRequest& request,
                               CreateTransformCompleter::Sync& completer) {
  CreateTransform(request.transform_id());
}

void Flatland::CreateTransform(TransformId transform_identifier) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "CreateTransform called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (transforms_.count(transform_id)) {
    error_reporter_->ERROR() << "CreateTransform called with pre-existing transform_id "
                             << transform_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  TransformHandle handle = transform_graph_.CreateTransform();
  FLATLAND_VERBOSE_LOG << "Flatland::CreateTransform() client-id: " << transform_id
                       << "  handle: " << handle;

  transforms_.insert({transform_id, handle});
}

void Flatland::SetTranslation(SetTranslationRequest& request,
                              SetTranslationCompleter::Sync& completer) {
  SetTranslation(request.transform_id(), request.translation());
}

void Flatland::SetTranslation(TransformId transform_identifier, fuchsia_math::Vec translation) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetTranslation called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetTranslation failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  matrices_[transform_kv->second].SetTranslation(translation);
}

void Flatland::SetOrientation(SetOrientationRequest& request,
                              SetOrientationCompleter::Sync& completer) {
  SetOrientation(request.transform_id(), request.orientation());
}

void Flatland::SetOrientation(TransformId transform_identifier,
                              fuchsia_ui_composition::Orientation orientation) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetOrientation called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetOrientation failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  matrices_[transform_kv->second].SetOrientation(orientation);
}

void Flatland::SetScale(SetScaleRequest& request, SetScaleCompleter::Sync& completer) {
  SetScale(request.transform_id(), request.scale());
}

void Flatland::SetScale(TransformId transform_identifier, fuchsia_math::VecF scale) {
  const uint64_t transform_id = transform_identifier.value();
  const float scale_x = scale.x();
  const float scale_y = scale.y();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetScale called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetScale failed, transform_id " << transform_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (scale_x == 0.f || scale_y == 0.f) {
    error_reporter_->ERROR() << "SetScale failed, zero values not allowed (" << scale_x << ", "
                             << scale_y << " ).";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (isinf(scale_x) || isinf(scale_y) || isnan(scale_x) || isnan(scale_y)) {
    error_reporter_->ERROR() << "SetScale failed, invalid scale values (" << scale_x << ", "
                             << scale_y << " ).";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  matrices_[transform_kv->second].SetScale(scale);
}

void Flatland::SetOpacity(SetOpacityRequest& request, SetOpacityCompleter::Sync& completer) {
  SetOpacity(request.transform_id().value(), request.value());
}

void Flatland::SetOpacity(TransformId transform_identifier, float opacity) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetOpacity called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (isinf(opacity) || isnan(opacity)) {
    error_reporter_->ERROR() << "SetOpacity failed, invalid opacity value " << opacity;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (opacity < 0.f || opacity > 1.f) {
    error_reporter_->ERROR() << "Opacity value is not within valid range [0,1].";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetOpacity failed, transform_id " << transform_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // Erase the value from the map since we store 1.f implicity.
  if (opacity == 1.f) {
    opacity_values_.erase(transform_kv->second);
  } else {
    opacity_values_[transform_kv->second] = opacity;
  }
}

void Flatland::SetClipBoundary(SetClipBoundaryRequest& request,
                               SetClipBoundaryCompleter::Sync& completer) {
  SetClipBoundary(request.transform_id(), std::move(request.rect()));
}

void Flatland::SetClipBoundary(TransformId transform_identifier,
                               fidl::Box<fuchsia_math::Rect> bounds) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetClipBoundary called with transform_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetClipBoundary failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // If the optional bounds are empty, then remove them.
  if (!bounds) {
    clip_regions_.erase(transform_kv->second);
    return;
  }

  SetClipBoundaryInternal(transform_kv->second, *bounds);
}

void Flatland::SetClipBoundaryInternal(TransformHandle handle, fuchsia_math::Rect bounds) {
  if (bounds.width() <= 0 || bounds.height() <= 0) {
    error_reporter_->ERROR() << "SetClipBoundary failed, width/height must both be positive "
                             << "(" << bounds.width() << ", " << bounds.height() << ")";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // The following overflow checks are based on those described here:
  //    https://wiki.sei.cmu.edu/confluence/display/c/INT32-C.
  //    +Ensure+that+operations+on+signed+integers+do+not+result+in+overflow
  if (((bounds.x() > 0) && (bounds.width() > (INT_MAX - bounds.x()))) ||
      ((bounds.x() < 0) && (bounds.width() < (INT_MIN - bounds.x())))) {
    error_reporter_->ERROR() << "SetClipBoundary failed, integer overflow on the X-axis.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (((bounds.y() > 0) && (bounds.height() > (INT_MAX - bounds.y()))) ||
      ((bounds.y() < 0) && (bounds.height() < (INT_MIN - bounds.y())))) {
    error_reporter_->ERROR() << "SetClipBoundary failed, integer overflow on the Y-axis.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  clip_regions_[handle] = fidl::NaturalToHLCPP(bounds);
}

std::vector<allocation::GlobalImageId> Flatland::ProcessDeadTransforms(
    const TransformGraph::TopologyData& data) {
  std::vector<allocation::GlobalImageId> images_to_release;
  for (const auto& dead_handle : data.dead_transforms) {
    matrices_.erase(dead_handle);

    // Gather all images corresponding to dead transforms.
    auto image_kv = image_metadatas_.find(dead_handle);
    if (image_kv != image_metadatas_.end()) {
      const auto image_id = image_kv->second.identifier;
      image_metadatas_.erase(image_kv);

      // FilledRects do not need to be released.
      if (image_id == allocation::kInvalidImageId)
        continue;

      // Remember all dead images so that we can release them in the destructor if necessary.
      // Typically this won't be necessary: we'll release them as soon as it is safe (roughly,
      // when the next present takes effect).
      images_to_release_->insert(image_id);

      images_to_release.push_back(image_id);
    }
  }

  return images_to_release;
}

void Flatland::AddChild(AddChildRequest& request, AddChildCompleter::Sync& completer) {
  AddChild(request.parent_transform_id(), request.child_transform_id());
}

void Flatland::AddChild(TransformId parent_transform_identifier,
                        TransformId child_transform_identifier) {
  const uint64_t parent_transform_id = parent_transform_identifier.value();
  const uint64_t child_transform_id = child_transform_identifier.value();

  if (parent_transform_id == kInvalidId || child_transform_id == kInvalidId) {
    error_reporter_->ERROR() << "AddChild called with transform_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto parent_global_kv = transforms_.find(parent_transform_id);
  auto child_global_kv = transforms_.find(child_transform_id);

  if (parent_global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "AddChild failed, parent_transform_id " << parent_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (child_global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "AddChild failed, child_transform_id " << child_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  bool added = transform_graph_.AddChild(parent_global_kv->second, child_global_kv->second);

  if (!added) {
    error_reporter_->ERROR() << "AddChild failed, connection already exists between parent "
                             << parent_transform_id << " and child " << child_transform_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }
}

void Flatland::RemoveChild(RemoveChildRequest& request, RemoveChildCompleter::Sync& completer) {
  RemoveChild(request.parent_transform_id(), request.child_transform_id());
}

void Flatland::RemoveChild(TransformId parent_transform_identifier,
                           TransformId child_transform_identifier) {
  const uint64_t parent_transform_id = parent_transform_identifier.value();
  const uint64_t child_transform_id = child_transform_identifier.value();

  if (parent_transform_id == kInvalidId || child_transform_id == kInvalidId) {
    error_reporter_->ERROR() << "RemoveChild failed, transform_id " << parent_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto parent_global_kv = transforms_.find(parent_transform_id);
  auto child_global_kv = transforms_.find(child_transform_id);

  if (parent_global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "RemoveChild failed, parent_transform_id " << parent_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (child_global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "RemoveChild failed, child_transform_id " << child_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  bool removed = transform_graph_.RemoveChild(parent_global_kv->second, child_global_kv->second);

  if (!removed) {
    error_reporter_->ERROR() << "RemoveChild failed, connection between parent "
                             << parent_transform_id << " and child " << child_transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }
}

void Flatland::ReplaceChildren(ReplaceChildrenRequest& request,
                               ReplaceChildrenCompleter::Sync& completer) {
  ReplaceChildren(request.parent_transform_id(), request.new_child_transform_ids());
}

void Flatland::ReplaceChildren(TransformId parent_transform_identifier,
                               const std::vector<TransformId>& new_child_transform_ids) {
  const uint64_t parent_transform_id = parent_transform_identifier.value();
  if (parent_transform_id == kInvalidId) {
    error_reporter_->ERROR() << "ReplaceChildren failed, parent transform_id "
                             << parent_transform_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto parent_global_kv = transforms_.find(parent_transform_id);
  if (parent_global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "ReplaceChildren failed, parent transform_id "
                             << parent_transform_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  std::vector<TransformHandle> children;
  for (auto child_transform_identifier : new_child_transform_ids) {
    const uint64_t child_transform_id = child_transform_identifier.value();
    if (child_transform_id == kInvalidId) {
      error_reporter_->ERROR() << "ReplaceChildren failed, child transform_id "
                               << child_transform_id << " not found";
      CloseConnection(FlatlandError::kBadOperation);
      return;
    }

    auto child_global_kv = transforms_.find(child_transform_id);
    if (child_global_kv == transforms_.end()) {
      error_reporter_->ERROR() << "ReplaceChildren failed, child transform_id "
                               << child_transform_id << " not found";
      CloseConnection(FlatlandError::kBadOperation);
      return;
    }
    children.push_back(child_global_kv->second);
  }

  bool replaced = transform_graph_.ReplaceChildren(parent_global_kv->second, children);
  if (!replaced) {
    error_reporter_->ERROR()
        << "ReplaceChildren failed, cannot add duplicate children to the same parent: "
        << parent_transform_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }
}

void Flatland::SetRootTransform(SetRootTransformRequest& request,
                                SetRootTransformCompleter::Sync& completer) {
  SetRootTransform(request.transform_id());
}

void Flatland::SetRootTransform(TransformId transform_identifier) {
  const uint64_t transform_id = transform_identifier.value();

  // SetRootTransform(0) is special -- it only clears the existing root transform.
  if (transform_id == kInvalidId) {
    transform_graph_.ClearChildren(local_root_);
    return;
  }

  const auto global_kv = transforms_.find(transform_id);
  if (global_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetRootTransform failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  transform_graph_.ClearChildren(local_root_);

  bool added = transform_graph_.AddChild(local_root_, global_kv->second);
  FX_DCHECK(added);

  root_transform_ = global_kv->second;
}

void Flatland::CreateViewport(CreateViewportRequest& request,
                              CreateViewportCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Flatland::CreateViewport", "debug_name", TA_STRING(debug_name_.c_str()));

  CreateViewport(request.viewport_id(), std::move(request.token()), std::move(request.properties()),
                 std::move(request.child_view_watcher()));
}

void Flatland::CreateViewport(
    ContentId viewport_id, fuchsia_ui_views::ViewportCreationToken token,
    fuchsia_ui_composition::ViewportProperties properties,
    fidl::ServerEnd<fuchsia_ui_composition::ChildViewWatcher> child_view_watcher) {
  const uint64_t link_id = viewport_id.value();

  // Attempting to link with an invalid token will never succeed, so its better to fail early and
  // immediately close the link connection.
  if (!token.value().is_valid()) {
    error_reporter_->ERROR() << "CreateViewport failed, ViewportCreationToken was invalid";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (!properties.logical_size().has_value()) {
    error_reporter_->ERROR()
        << "CreateViewport must be provided a ViewportProperties with a logical size";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (auto error = ValidateViewportProperties(properties)) {
    error_reporter_->ERROR() << "CreateViewport failed: " << *error;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  SetViewportPropertiesMissingDefaults(properties, properties.logical_size().value(),
                                       /*inset*/ {0, 0, 0, 0});

  if (link_id == kInvalidId) {
    error_reporter_->ERROR() << "CreateViewport called with ContentId zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (content_handles_.count(link_id)) {
    error_reporter_->ERROR() << "CreateViewport called with existing ContentId " << link_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FX_DCHECK(link_system_);

  // The ViewportProperties and ChildViewWatcherImpl live on a handle from this Flatland instance.
  const auto parent_transform_handle = transform_graph_.CreateTransform();

  // We can initialize the Link importer immediately, since no state changes actually occur before
  // the feed-forward portion of this method. We also forward the initial ViewportProperties
  // through the LinkSystem immediately, so the child can receive them as soon as possible.
  LinkSystem::LinkToChild link_to_child = link_system_->CreateLinkToChild(
      dispatcher_holder_, fidl::NaturalToHLCPP(token), fidl::NaturalToHLCPP(properties),
      fidl::NaturalToHLCPP(child_view_watcher), parent_transform_handle,
      [ref = weak_from_this(), weak_dispatcher_holder = std::weak_ptr<utils::DispatcherHolder>(
                                   dispatcher_holder_)](const std::string& error_log) {
        if (auto dispatcher_holder = weak_dispatcher_holder.lock()) {
          FX_CHECK(dispatcher_holder->dispatcher() == async_get_default_dispatcher())
              << "Link protocol error reported on the wrong dispatcher.";
        }
        if (auto impl = ref.lock())
          impl->ReportLinkProtocolError(error_log);
      });

  // This is the feed-forward portion of the method. Here, we add the link to the map, and
  // initialize its layout with the desired properties. The Link will not actually result in
  // additions to the Transform hierarchy until it is added to a Transform.
  {
    const bool child_added = transform_graph_.AddChild(link_to_child.parent_transform_handle,
                                                       link_to_child.internal_link_handle);
    FX_DCHECK(child_added);
  }

  FLATLAND_VERBOSE_LOG << "Flatland::CreateViewport() in " << local_root_
                       << " parent_transform_handle: " << link_to_child.parent_transform_handle
                       << " internal_link_handle: " << link_to_child.internal_link_handle;

  // Default the link size to the logical size, which is just an identity scale matrix, so
  // that future logical size changes will result in the correct scale matrix.
  const SizeU size = *properties.logical_size();

  content_handles_[link_id] = link_to_child.parent_transform_handle;
  links_to_children_[link_to_child.parent_transform_handle] = {.link = std::move(link_to_child),
                                                               .properties = std::move(properties)};

  // Set clip bounds on the transform associated with the viewport content.
  SetClipBoundaryInternal(parent_transform_handle,
                          fuchsia_math::Rect{}
                              .x(0)
                              .y(0)
                              .width(static_cast<int32_t>(size.width()))
                              .height(static_cast<int32_t>(size.height())));
}

void Flatland::CreateImage(CreateImageRequest& request, CreateImageCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Flatland::CreateImage", "debug_name", TA_STRING(debug_name_.c_str()));

  CreateImage(request.image_id(), std::move(request.import_token()), request.vmo_index(),
              std::move(request.properties()));
}

void Flatland::CreateImage(ContentId image_id,
                           fuchsia_ui_composition::BufferCollectionImportToken import_token,
                           uint32_t vmo_index, fuchsia_ui_composition::ImageProperties properties) {
  if (image_id.value() == kInvalidId) {
    error_reporter_->ERROR() << "CreateImage called with image_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (content_handles_.count(image_id.value())) {
    error_reporter_->ERROR() << "CreateImage called with pre-existing image_id "
                             << image_id.value();
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  const BufferCollectionId global_collection_id = fsl::GetRelatedKoid(import_token.value().get());

  // Check if there is a valid peer.
  if (global_collection_id == ZX_KOID_INVALID) {
    error_reporter_->ERROR() << "CreateImage called with no valid export token";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (!properties.size().has_value()) {
    error_reporter_->ERROR() << "CreateImage failed, ImageProperties did not specify size";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (!properties.size()->width()) {
    error_reporter_->ERROR() << "CreateImage failed, ImageProperties did not specify a width";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (!properties.size()->height()) {
    error_reporter_->ERROR() << "CreateImage failed, ImageProperties did not specify a height";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  allocation::ImageMetadata metadata;
  metadata.identifier = allocation::GenerateUniqueImageId();
  metadata.collection_id = global_collection_id;
  metadata.vmo_index = vmo_index;
  metadata.width = properties.size()->width();
  metadata.height = properties.size()->height();
  metadata.blend_mode = fuchsia_ui_composition::BlendMode::kSrc;

  for (uint32_t i = 0; i < buffer_collection_importers_.size(); i++) {
    auto& importer = buffer_collection_importers_[i];

    // TODO(https://fxbug.dev/42140615): Give more detailed errors.
    auto result =
        importer->ImportBufferImage(metadata, allocation::BufferCollectionUsage::kClientImage);
    if (!result) {
      // If this importer fails, we need to release the image from
      // all of the importers that it passed on. Luckily we can do
      // this right here instead of waiting for a fence since we know
      // this image isn't being used by anything yet.
      for (uint32_t j = 0; j < i; j++) {
        buffer_collection_importers_[j]->ReleaseBufferImage(metadata.identifier);
      }

      error_reporter_->ERROR() << "Importer could not import image.";
      CloseConnection(FlatlandError::kBadOperation);
      return;
    }
  }

  // Now that we've successfully been able to import the image into the importers,
  // we can now create a handle for it in the transform graph, and add the metadata
  // to our map.
  auto handle = transform_graph_.CreateTransform();
  content_handles_[image_id.value()] = handle;
  image_metadatas_[handle] = metadata;

  // Set the default sample region of the image to be the full image.
  SetImageSampleRegion(image_id, {0, 0, static_cast<float>(properties.size()->width()),
                                  static_cast<float>(properties.size()->height())});

  // Set the default destination region of the image to be the full image.
  SetImageDestinationSize(image_id, properties.size().value());

  FLATLAND_VERBOSE_LOG << "Flatland::CreateImage" << handle << " for " << local_root_
                       << " size:" << properties.size()->width() << "x"
                       << properties.size()->height();
}

void Flatland::SetImageSampleRegion(SetImageSampleRegionRequest& request,
                                    SetImageSampleRegionCompleter::Sync& completer) {
  SetImageSampleRegion(request.image_id(), request.rect());
}

void Flatland::SetImageSampleRegion(ContentId image_id, fuchsia_math::RectF rect) {
  if ((image_id.value()) == kInvalidId) {
    error_reporter_->ERROR() << "SetImageSampleRegion called with content id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  const auto content_kv = content_handles_.find((image_id.value()));
  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetImageSampleRegion called with non-existent image_id "
                             << (image_id.value());
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  const auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "SetImageSampleRegion called on non-image content.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // The provided sample region needs to be within the bounds of the image.
  {
    const auto& metadata = image_kv->second;
    const auto image_width = static_cast<float>(metadata.width);
    const auto image_height = static_cast<float>(metadata.height);
    // This clamping is required in cases where (x+width>image_width) or (y+height>image_height)
    // by a small epsilon. The downstream code expects these numbers to be within the
    // (image_width, image_height) limits, so we only clamp the positive differences. The root
    // cause is the precision errors in floating point arithmetic when a client tries to calculate
    // floats within pixel space.
    // TODO(https://fxbug.dev/42082599): Remove floating point precision error checks and use
    // uints instead.
    ClampIfNear(&rect.width(), rect.x() + rect.width() - image_width);
    ClampIfNear(&rect.height(), rect.y() + rect.height() - image_height);
    if (rect.x() < 0.f || rect.width() < 0.f || (rect.x() + rect.width()) > image_width ||
        rect.y() < 0.f || rect.height() < 0.f || (rect.y() + rect.height()) > image_height) {
      error_reporter_->ERROR() << "SetImageSampleRegion rect " << rect
                               << " out of bounds for image (" << image_width << ", "
                               << image_height << ")";
      CloseConnection(FlatlandError::kBadOperation);
      return;
    }
  }

  image_sample_regions_[content_kv->second] = fidl::NaturalToHLCPP(rect);
}

void Flatland::SetImageDestinationSize(SetImageDestinationSizeRequest& request,
                                       SetImageDestinationSizeCompleter::Sync& completer) {
  SetImageDestinationSize(request.image_id(), request.size());
}

void Flatland::SetImageDestinationSize(ContentId image_id, fuchsia_math::SizeU size) {
  if (image_id.value() == kInvalidId) {
    error_reporter_->ERROR() << "SetImageSize called with image_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(image_id.value());

  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetImageSize called with non-existent image_id "
                             << image_id.value();
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "SetImageSize called on non-image content  " << image_id.value();
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  matrices_[content_kv->second].SetScale(
      {static_cast<float>(size.width()), static_cast<float>(size.height())});
}

void Flatland::SetImageBlendingFunction(SetImageBlendingFunctionRequest& request,
                                        SetImageBlendingFunctionCompleter::Sync& completer) {
  SetImageBlendingFunction(request.image_id(), request.blend_mode());
}

void Flatland::SetImageBlendingFunction(ContentId image_identifier,
                                        fuchsia_ui_composition::BlendMode blend_mode) {
  const uint64_t image_id = image_identifier.value();

  if (image_id == kInvalidId) {
    error_reporter_->ERROR() << "SetImageBlendingFunction called with content id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(image_id);
  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetImageBlendingFunction called with non-existent image_id "
                             << image_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "SetImageBlendingFunction called on non-image content.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  image_kv->second.blend_mode = blend_mode;
}

void Flatland::SetImageFlip(SetImageFlipRequest& request, SetImageFlipCompleter::Sync& completer) {
  SetImageFlip(request.image_id(), request.flip());
}

void Flatland::SetImageFlip(ContentId image_identifier, fuchsia_ui_composition::ImageFlip flip) {
  const uint64_t image_id = image_identifier.value();

  if (image_id == kInvalidId) {
    error_reporter_->ERROR() << "SetImageFlip called with content id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(image_id);
  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetImageFlip called with non-existent image_id " << image_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "SetImageFlip called on non-image content.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  image_kv->second.flip = flip;
}

void Flatland::CreateFilledRect(CreateFilledRectRequest& request,
                                CreateFilledRectCompleter::Sync& completer) {
  CreateFilledRect(request.rect_id());
}

void Flatland::CreateFilledRect(ContentId rect_identifier) {
  const uint64_t rect_id = rect_identifier.value();

  if (rect_id == kInvalidId) {
    error_reporter_->ERROR() << "CreateFilledRect called with rect_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (content_handles_.count(rect_id)) {
    error_reporter_->ERROR() << "CreateFilledRect called with pre-existing content id " << rect_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  allocation::ImageMetadata metadata;
  // allocation::kInvalidImageId is overloaded in the renderer to signal that a
  // default 1x1 white texture should be applied to this rectangle.
  metadata.identifier = allocation::kInvalidImageId;
  metadata.blend_mode = fuchsia_ui_composition::BlendMode::kSrc;

  // Now that we've successfully been able to import the image into the importers,
  // we can now create a handle for it in the transform graph, and add the metadata
  // to our map.
  auto handle = transform_graph_.CreateTransform();
  content_handles_[rect_id] = handle;
  image_metadatas_[handle] = metadata;
}

void Flatland::SetSolidFill(SetSolidFillRequest& request, SetSolidFillCompleter::Sync& completer) {
  SetSolidFill(request.rect_id(), request.color(), request.size());
}

void Flatland::SetSolidFill(ContentId rect_identifier, fuchsia_ui_composition::ColorRgba color,
                            fuchsia_math::SizeU size) {
  const uint64_t rect_id = rect_identifier.value();

  if (rect_id == kInvalidId) {
    error_reporter_->ERROR() << "SetSolidFill called with rect_id 0";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(rect_id);

  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetSolidFill called with non-existent rect_id " << rect_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "Missing metadada for rect with id  " << rect_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (color.red() < 0.f || color.red() > 1.f || isinf(color.red()) || isnan(color.red()) ||
      color.green() < 0.f || color.green() > 1.f || isinf(color.green()) || isnan(color.green()) ||
      color.blue() < 0.f || color.blue() > 1.f || isinf(color.blue()) || isnan(color.blue()) ||
      color.alpha() < 0.f || color.alpha() > 1.f || isinf(color.alpha()) || isnan(color.alpha())) {
    error_reporter_->ERROR() << "Invalid color channel(s) (" << color.red() << ", " << color.green()
                             << ", " << color.blue() << ", " << color.alpha() << ")";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  image_kv->second.blend_mode = color.alpha() < 1.f ? fuchsia_ui_composition::BlendMode::kSrcOver
                                                    : fuchsia_ui_composition::BlendMode::kSrc;
  image_kv->second.collection_id = allocation::kInvalidId;
  image_kv->second.identifier = allocation::kInvalidImageId;
  image_kv->second.multiply_color = {color.red(), color.green(), color.blue(), color.alpha()};
  matrices_[content_kv->second].SetScale(
      {static_cast<float>(size.width()), static_cast<float>(size.height())});
}

void Flatland::ReleaseFilledRect(ReleaseFilledRectRequest& request,
                                 ReleaseFilledRectCompleter::Sync& completer) {
  ReleaseFilledRect(request.rect_id());
}

void Flatland::ReleaseFilledRect(ContentId rect_identifier) {
  const uint64_t rect_id = rect_identifier.value();

  if (rect_id == kInvalidId) {
    error_reporter_->ERROR() << "ReleaseFilledRect called with rect_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(rect_id);

  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "ReleaseFilledRect failed, rect_id " << rect_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);

  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "ReleaseFilledRect failed, content_id " << rect_id
                             << " has no metadata.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  bool erased_from_graph = transform_graph_.ReleaseTransform(content_kv->second);
  FX_DCHECK(erased_from_graph);

  // Even though the handle is released, it may still be referenced by client Transforms. The
  // image_metadatas_ map preserves the entry until it shows up in the dead_transforms list.
  content_handles_.erase(rect_id);
}

void Flatland::SetImageOpacity(SetImageOpacityRequest& request,
                               SetImageOpacityCompleter::Sync& completer) {
  SetImageOpacity(request.image_id(), request.val());
}

void Flatland::SetImageOpacity(ContentId image_identifier, float opacity) {
  const uint64_t image_id = image_identifier.value();

  if (image_id == kInvalidId) {
    error_reporter_->ERROR() << "SetImageOpacity called with invalid image_id";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(image_id);
  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetImageOpacity called with non-existent image_id " << image_id;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);
  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "SetImageOpacity called on non-rectangle content.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto& metadata = image_kv->second;
  if (metadata.identifier == allocation::kInvalidImageId) {
    error_reporter_->ERROR() << "SetImageOpacity called on solid color content.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (opacity < 0.f || opacity > 1.f) {
    error_reporter_->ERROR() << "Opacity value is not within valid range [0,1].";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // Opacity is stored as the alpha channel of the multiply color.
  metadata.multiply_color[3] = opacity;
}

void Flatland::SetHitRegions(SetHitRegionsRequest& request,
                             SetHitRegionsCompleter::Sync& completer) {
  SetHitRegions(request.transform_id(), std::move(request.regions()));
}

void Flatland::SetHitRegions(TransformId transform_identifier,
                             std::vector<fuchsia_ui_composition::HitRegion> regions) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetHitRegions called with invalid transform ID";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);
  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetHitRegions failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  // Validate |regions|.
  for (auto& region : regions) {
    auto& rect = region.region();

    if (rect.width() < 0 || rect.height() < 0) {
      error_reporter_->ERROR() << "SetHitRegions failed, contains invalid (negative) dimensions: ("
                               << rect.width() << "," << rect.height() << ")";
      CloseConnection(FlatlandError::kBadOperation);
      return;
    }
  }

  // Reformat into internal type.
  std::vector<flatland::HitRegion> list;
  for (auto& region : regions) {
    list.emplace_back(fidl::NaturalToHLCPP(region.region()),
                      fidl::NaturalToHLCPP(region.hit_test()));
  }
  hit_regions_[transform_kv->second] = list;
}

void Flatland::SetInfiniteHitRegion(SetInfiniteHitRegionRequest& request,
                                    SetInfiniteHitRegionCompleter::Sync& completer) {
  SetInfiniteHitRegion(request.transform_id(), std::move(request.hit_test()));
}

void Flatland::SetInfiniteHitRegion(TransformId transform_identifier,
                                    fuchsia_ui_composition::HitTestInteraction hit_test) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetHitRegions called with invalid transform ID";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);
  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetHitRegions failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  hit_regions_[transform_kv->second] = {
      flatland::HitRegion::Infinite(fidl::NaturalToHLCPP(hit_test))};
}

void Flatland::SetContent(SetContentRequest& request, SetContentCompleter::Sync& completer) {
  SetContent(request.transform_id(), request.content_id());
}

void Flatland::SetContent(TransformId transform_identifier, ContentId content_identifier) {
  const uint64_t transform_id = transform_identifier.value();
  const uint64_t content_id = content_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "SetContent called with transform_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "SetContent failed, transform_id " << transform_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  if (content_id == kInvalidId) {
    transform_graph_.ClearPriorityChild(transform_kv->second);
    FLATLAND_VERBOSE_LOG << "Flatland::SetContent() cleared content for transform: "
                         << transform_kv->second;
    return;
  }

  auto handle_kv = content_handles_.find(content_id);

  if (handle_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetContent failed, content_id " << content_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FLATLAND_VERBOSE_LOG << "Flatland::SetContent(" << transform_kv->second << ","
                       << handle_kv->second << ")";

  transform_graph_.SetPriorityChild(transform_kv->second, handle_kv->second);
}

void Flatland::SetViewportProperties(SetViewportPropertiesRequest& request,
                                     SetViewportPropertiesCompleter::Sync& completer) {
  SetViewportProperties(request.viewport_id(), std::move(request.properties()));
}

void Flatland::SetViewportProperties(ContentId viewport_id,
                                     fuchsia_ui_composition::ViewportProperties properties) {
  const uint64_t link_id = viewport_id.value();

  if (link_id == kInvalidId) {
    error_reporter_->ERROR() << "SetViewportProperties called with link_id zero.";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  const auto content_kv = content_handles_.find(link_id);
  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "SetViewportProperties failed, link_id " << link_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  const auto viewport_handle = content_kv->second;

  auto link_kv = links_to_children_.find(viewport_handle);
  if (link_kv == links_to_children_.end()) {
    error_reporter_->ERROR() << "SetViewportProperties failed, content_id " << link_id
                             << " is not a Link";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  LinkToChildData& link_data = link_kv->second;
  if (!link_data.link.importer.valid()) {
    // Other side of the Viewport has been invalidated and the Viewport should be released.
    // Calling SetViewportProperties() must still be allowed since the client may not have gotten
    // the destruction message yet.
    return;
  }

  if (auto error = ValidateViewportProperties(properties)) {
    error_reporter_->ERROR() << "SetViewportProperties failed: " << *error;
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FX_DCHECK(link_data.properties.logical_size().has_value());
  FX_DCHECK(link_data.properties.inset().has_value());
  SetViewportPropertiesMissingDefaults(properties, *link_data.properties.logical_size(),
                                       *link_data.properties.inset());

  // Update the clip boundaries when the properties change.
  SetClipBoundaryInternal(viewport_handle,
                          fuchsia_math::Rect{}
                              .x(0)
                              .y(0)
                              .width(static_cast<int32_t>(properties.logical_size()->width()))
                              .height(static_cast<int32_t>(properties.logical_size()->height())));

  link_data.properties = properties;
  link_system_->UpdateViewportPropertiesFor(viewport_handle, fidl::NaturalToHLCPP(properties));
}

void Flatland::ReleaseTransform(ReleaseTransformRequest& request,
                                ReleaseTransformCompleter::Sync& completer) {
  ReleaseTransform(request.transform_id());
}

void Flatland::ReleaseTransform(TransformId transform_identifier) {
  const uint64_t transform_id = transform_identifier.value();

  if (transform_id == kInvalidId) {
    error_reporter_->ERROR() << "ReleaseTransform called with transform_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto transform_kv = transforms_.find(transform_id);

  if (transform_kv == transforms_.end()) {
    error_reporter_->ERROR() << "ReleaseTransform failed, transform_id " << transform_id
                             << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  bool erased_from_graph = transform_graph_.ReleaseTransform(transform_kv->second);
  FX_DCHECK(erased_from_graph);
  transforms_.erase(transform_kv);
}

void Flatland::ReleaseViewport(ReleaseViewportRequest& request,
                               ReleaseViewportCompleter::Sync& completer) {
  ReleaseViewport(
      request.viewport_id(),
      [completer = completer.ToAsync()](fuchsia_ui_views::ViewportCreationToken token) mutable {
        completer.Reply(std::move(token));
      });
}

void Flatland::ReleaseViewport(
    ContentId viewport_id, fit::function<void(fuchsia_ui_views::ViewportCreationToken)> completer) {
  const uint64_t link_id = viewport_id.value();

  if (link_id == kInvalidId) {
    error_reporter_->ERROR() << "ReleaseViewport called with link_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(link_id);

  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "ReleaseViewport failed, link_id " << link_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto link_kv = links_to_children_.find(content_kv->second);

  if (link_kv == links_to_children_.end()) {
    error_reporter_->ERROR() << "ReleaseViewport failed, content_id " << link_id
                             << " is not a Link";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  LinkToChildData& link_data = link_kv->second;

  // Deleting the LinkToChild's |parent_transform_handle| effectively deletes the link from
  // the local topology, even if the link object itself is not deleted.
  {
    const bool child_removed = transform_graph_.RemoveChild(link_data.link.parent_transform_handle,
                                                            link_data.link.internal_link_handle);
    FX_DCHECK(child_removed);
    const bool content_released =
        transform_graph_.ReleaseTransform(link_data.link.parent_transform_handle);
    FX_DCHECK(content_released);
  }

  // Move the old child link into the delayed operation so that the ContentId is immediately free
  // for re-use, but it doesn't get deleted until after the new UberStruct is published.
  auto link_to_child = std::move(link_data);
  links_to_children_.erase(content_kv->second);
  content_handles_.erase(content_kv);

  // Delay the actual destruction of the link until the next Present().
  pending_link_operations_.push_back(
      [link_to_child = std::move(link_to_child), completer = std::move(completer)]() mutable {
        ViewportCreationToken return_token;

        // If the link is still valid, return the original token. If not, create an orphaned
        // zx::channel and return it since the ObjectLinker does not retain the orphaned token.
        auto link_token = link_to_child.link.importer.ReleaseToken();
        if (link_token.has_value()) {
          return_token.value(zx::channel(std::move(link_token.value())));
        } else {
          // |peer_token| immediately falls out of scope, orphaning |return_token|.
          zx::channel peer_token;
          zx::channel::create(0, &return_token.value(), &peer_token);
        }

        completer(std::move(return_token));
      });
}

void Flatland::ReleaseImage(ReleaseImageRequest& request, ReleaseImageCompleter::Sync& completer) {
  ReleaseImage(request.image_id());
}

void Flatland::ReleaseImage(ContentId image_identifier) {
  const uint64_t image_id = image_identifier.value();

  if (image_id == kInvalidId) {
    error_reporter_->ERROR() << "ReleaseImage called with image_id zero";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto content_kv = content_handles_.find(image_id);

  if (content_kv == content_handles_.end()) {
    error_reporter_->ERROR() << "ReleaseImage failed, image_id " << image_id << " not found";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  auto image_kv = image_metadatas_.find(content_kv->second);

  if (image_kv == image_metadatas_.end()) {
    error_reporter_->ERROR() << "ReleaseImage failed, content_id " << image_id
                             << " is not an Image";
    CloseConnection(FlatlandError::kBadOperation);
    return;
  }

  FLATLAND_VERBOSE_LOG << "Flatland::ReleaseImage" << content_kv->second << " for " << local_root_;

  bool erased_from_graph = transform_graph_.ReleaseTransform(content_kv->second);
  FX_DCHECK(erased_from_graph);

  // Even though the handle is released, it may still be referenced by client Transforms. The
  // image_metadatas_ map preserves the entry until it shows up in the dead_transforms list.
  content_handles_.erase(image_id);
}

void Flatland::SetDebugName(SetDebugNameRequest& request, SetDebugNameCompleter::Sync& completer) {
  std::string name(std::move(request.name()));

  TRACE_INSTANT("gfx", "Flatland::SetDebugName()", TRACE_SCOPE_PROCESS, "name",
                TA_STRING(name.c_str()));

  SetDebugName(std::move(name));
}

void Flatland::SetDebugName(std::string name) {
  std::stringstream stream;
  if (!name.empty())
    stream << "Flatland client(" << name << "): ";

  FLATLAND_VERBOSE_LOG << "Flatland::SetDebugName() to " << stream.str() << " for " << local_root_
                       << " " << this;

  error_reporter_->SetPrefix(stream.str());
  debug_name_ = std::move(name);
}

void Flatland::OnNextFrameBegin(uint32_t additional_present_credits,
                                FuturePresentationInfos presentation_infos) {
  TRACE_DURATION("gfx", "Flatland::OnNextFrameBegin");
  present_credits_ += additional_present_credits;

  // Only send an `OnNextFrameBegin` event if the client has at least one present credit. It is
  // guaranteed that this won't stall clients because the current policy is to always return
  // present tokens upon processing them. If and when a new policy is adopted, we should take care
  // to ensure this guarantee is upheld.
  if (binding_data_) {
    if (present_credits_ > 0) {
      binding_data_->SendOnNextFrameBegin(additional_present_credits,
                                          std::move(presentation_infos));
    }
  }
}

void Flatland::OnFramePresented(const std::map<scheduling::PresentId, zx::time>& latched_times,
                                scheduling::PresentTimestamps present_times) {
  TRACE_DURATION("gfx", "Flatland::OnFramePresented");
  // TODO(https://fxbug.dev/42141795): remove `num_presents_allowed` from this event.  Clients
  // should obtain this information from OnPresentProcessedValues().
  present2_helper_.OnPresented(latched_times, present_times, /*num_presents_allowed=*/0);
}

TransformHandle Flatland::GetRoot() const {
  return link_to_parent_ ? link_to_parent_->child_transform_handle : local_root_;
}

std::optional<TransformHandle> Flatland::GetContentHandle(ContentId content_id) const {
  auto handle_kv = content_handles_.find(content_id.value());
  if (handle_kv == content_handles_.end()) {
    return std::nullopt;
  }
  return handle_kv->second;
}

// For validating properties associated with transforms in tests only. If |transform_id| does not
// exist for this Flatland instance, returns std::nullopt.
std::optional<TransformHandle> Flatland::GetTransformHandle(TransformId transform_id) const {
  auto handle_kv = transforms_.find(transform_id.value());
  if (handle_kv == transforms_.end()) {
    return std::nullopt;
  }
  return handle_kv->second;
}

void Flatland::SetErrorReporter(std::unique_ptr<scenic_impl::ErrorReporter> error_reporter) {
  error_reporter_ = std::move(error_reporter);
}

scheduling::SessionId Flatland::GetSessionId() const { return session_id_; }

void Flatland::OnFidlClosed(fidl::UnbindInfo unbind_info) {
  if (!unbind_info.is_user_initiated()) {
    FX_LOGS(INFO) << "Flatland::OnFidlClosed() because: " << unbind_info.FormatDescription();
  }

  binding_data_.reset();
}

void Flatland::ReportLinkProtocolError(const std::string& error_log) {
  error_reporter_->ERROR() << error_log;
  link_protocol_error_ = true;
}

void Flatland::CloseConnection(FlatlandError error) {
  if (binding_data_) {
    FLATLAND_VERBOSE_LOG << "Flatland::CloseConnection(" << error
                         << ")  in session: " << session_id_;

    binding_data_->CloseConnection(error);
    binding_data_.reset();
  }
}

// MatrixData function implementations

// static
float Flatland::MatrixData::GetOrientationAngle(fuchsia_ui_composition::Orientation orientation) {
  // The matrix is specified in view-space coordinates, in which the +y axis points downwards (not
  // upwards). Rotations which are specified as counter-clockwise must actually occur in a
  // clockwise fashion in this coordinate space (a vector on the +x axis rotates towards -y axis
  // to give the appearance of a counter-clockwise rotation).
  switch (orientation) {
    case Orientation::kCcw0Degrees:
      return 0.f;
    case Orientation::kCcw90Degrees:
      return -glm::half_pi<float>();
    case Orientation::kCcw180Degrees:
      return -glm::pi<float>();
    case Orientation::kCcw270Degrees:
      return -glm::three_over_two_pi<float>();
  }
}

void Flatland::MatrixData::SetTranslation(Vec translation) {
  translation_.x = static_cast<float>(translation.x());
  translation_.y = static_cast<float>(translation.y());
  RecomputeMatrix();
}

void Flatland::MatrixData::SetOrientation(fuchsia_ui_composition::Orientation orientation) {
  angle_ = GetOrientationAngle(orientation);

  RecomputeMatrix();
}

void Flatland::MatrixData::SetScale(VecF scale) {
  scale_.x = scale.x();
  scale_.y = scale.y();
  RecomputeMatrix();
}

void Flatland::MatrixData::RecomputeMatrix() {
  // Manually compose the matrix rather than use glm transformations since the order of operations
  // is always the same. glm matrices are column-major.
  float* vals = static_cast<float*>(glm::value_ptr(matrix_));

  // Translation in the third column.
  vals[6] = translation_.x;
  vals[7] = translation_.y;

  // Rotation and scale combined into the first two columns.
  const float s = sin(angle_);
  const float c = cos(angle_);

  vals[0] = c * scale_.x;
  vals[1] = s * scale_.x;
  vals[3] = -1.f * s * scale_.y;
  vals[4] = c * scale_.y;
}

glm::mat3 Flatland::MatrixData::GetMatrix() const { return matrix_; }

}  // namespace flatland
