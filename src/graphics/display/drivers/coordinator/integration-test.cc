// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/wire/array.h>
#include <lib/fit/result.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>
#include <optional>
#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/coordinator/testing/mock-coordinator-listener.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"
#include "src/graphics/display/lib/api-types/cpp/mode.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"
#include "src/graphics/display/lib/driver-utils/post-task.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

namespace {

// Cached information about a display reported by the coordinator.
struct TestDisplayInfo {
 public:
  static TestDisplayInfo From(const fuchsia_hardware_display::wire::Info& fidl_display_info);

  display::DisplayId id;

  // Represents an image that covers the entire display.
  display::ImageMetadata fullscreen_image_metadata;
};

// static
TestDisplayInfo TestDisplayInfo::From(
    const fuchsia_hardware_display::wire::Info& fidl_display_info) {
  const display::DisplayId display_id = display::ToDisplayId(fidl_display_info.id);
  ZX_ASSERT(display_id != display::kInvalidDisplayId);

  ZX_ASSERT(!fidl_display_info.modes.empty());
  display::Mode display_mode = display::Mode::From(fidl_display_info.modes[0]);

  const display::ImageMetadata fullscreen_image_metadata = display::ImageMetadata({
      .width = display_mode.active_area().width(),
      .height = display_mode.active_area().height(),
      .tiling_type = display::ImageTilingType::kLinear,
  });

  return TestDisplayInfo{
      .id = display_id,
      .fullscreen_image_metadata = fullscreen_image_metadata,
  };
}

// Coordinator client state updated by the listener protocol.
//
// This class is thread-safe.
class TestClientState {
 public:
  TestClientState() = default;
  TestClientState(const TestClientState&) = delete;
  TestClientState& operator=(const TestClientState&) = delete;
  ~TestClientState() = default;

  // The returned count is guaranteed to be monotonically increasing across the
  // instance's lifetime.
  uint64_t vsync_count() const;

  display::ConfigStamp last_vsync_config_stamp() const;
  display::VsyncAckCookie last_vsync_ack_cookie() const;

  bool HasOwnershipAndValidDisplay() const;

  // The first connected display's ID.
  //
  // Crashes if no display is connected.
  display::DisplayId display_id() const;

  // Metadata for an image that fully covers the first connected display.
  //
  // Crashes if no display is connected.
  display::ImageMetadata FullscreenImageMetadata() const;

  // MockCoordinatorListener implementation
  void OnDisplaysChanged(std::span<const fuchsia_hardware_display::wire::Info> added_displays,
                         std::span<const display::DisplayId> removed_display_ids);
  void OnClientOwnershipChange(bool has_ownership);
  void OnVsync(display::DisplayId display_id, zx::time timestamp,
               display::ConfigStamp applied_config_stamp, display::VsyncAckCookie vsync_ack_cookie);

 private:
  // Locks all the state in this class.
  mutable std::mutex mutex_;

  std::vector<TestDisplayInfo> connected_displays_ __TA_GUARDED(mutex_);
  bool has_ownership_ __TA_GUARDED(mutex_) = false;
  uint64_t vsync_count_ TA_GUARDED(mutex_) = 0;
  display::VsyncAckCookie last_vsync_ack_cookie_ __TA_GUARDED(mutex_) =
      display::kInvalidVsyncAckCookie;
  display::ConfigStamp last_vsync_config_stamp_ __TA_GUARDED(mutex_);
};

uint64_t TestClientState::vsync_count() const {
  std::lock_guard lock(mutex_);
  return vsync_count_;
}

display::ConfigStamp TestClientState::last_vsync_config_stamp() const {
  std::lock_guard lock(mutex_);
  return last_vsync_config_stamp_;
}

display::VsyncAckCookie TestClientState::last_vsync_ack_cookie() const {
  std::lock_guard lock(mutex_);
  return last_vsync_ack_cookie_;
}

bool TestClientState::HasOwnershipAndValidDisplay() const {
  std::lock_guard lock(mutex_);
  return has_ownership_ && !connected_displays_.empty();
}

display::DisplayId TestClientState::display_id() const {
  std::lock_guard lock(mutex_);
  ZX_ASSERT(!connected_displays_.empty());
  return connected_displays_[0].id;
}

display::ImageMetadata TestClientState::FullscreenImageMetadata() const {
  std::lock_guard lock(mutex_);
  ZX_ASSERT(!connected_displays_.empty());
  return connected_displays_[0].fullscreen_image_metadata;
}

void TestClientState::OnDisplaysChanged(
    std::span<const fuchsia_hardware_display::wire::Info> added_displays,
    std::span<const display::DisplayId> removed_display_ids) {
  ZX_ASSERT(removed_display_ids.empty());

  std::lock_guard lock(mutex_);
  for (const fuchsia_hardware_display::wire::Info& added_display : added_displays) {
    connected_displays_.push_back(TestDisplayInfo::From(added_display));
  }
}

void TestClientState::OnClientOwnershipChange(bool has_ownership) {
  std::lock_guard lock(mutex_);
  has_ownership_ = has_ownership;
}

void TestClientState::OnVsync(display::DisplayId display_id, zx::time timestamp,
                              display::ConfigStamp applied_config_stamp,
                              display::VsyncAckCookie vsync_ack_cookie) {
  std::lock_guard lock(mutex_);
  ++vsync_count_;
  last_vsync_config_stamp_ = applied_config_stamp;
  if (vsync_ack_cookie != display::kInvalidVsyncAckCookie) {
    last_vsync_ack_cookie_ = vsync_ack_cookie;
  }
}

// Encapsulates boilerplate for driving the Coordinator via FIDL.
//
// This class is not thead-safe. Instances must be accessed on a single thread,
// or on a single synchronized dispatcher. Exception: both `state()` and the
// returned `TestClientState` instance can be accessed from any thread.
class TestFidlClient {
 public:
  struct EventInfo {
    display::EventId id;
    zx::event event;
  };

  struct LayerConfig {
    display::LayerId layer_id;
    display::ImageId image_id;
    std::optional<display::EventId> image_ready_wait_event_id;
  };

  // `sysmem` must outlive this instance.
  explicit TestFidlClient(const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>* sysmem);
  TestFidlClient(const TestFidlClient&) = delete;
  TestFidlClient& operator=(const TestFidlClient&) = delete;
  ~TestFidlClient();

  // Thread-safe.
  TestClientState& state() { return state_; }

  // `coordinator_listener_dispatcher` must be non-null and must be running
  // throughout the test.
  zx::result<> OpenCoordinator(
      const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
      ClientPriority client_priority, async_dispatcher_t* coordinator_listener_dispatcher);

  zx::result<> EnableVsyncEventDelivery();

  zx::result<> SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode virtcon_mode);
  zx::result<display::LayerId> CreateLayer();
  zx::result<> ImportImage(const display::ImageMetadata& image_metadata,
                           display::BufferId image_buffer_id, display::ImageId image_id);
  zx::result<> ImportEvent(zx::event event, display::EventId event_id);

  // The std::vector can be converted to std::span once we adopt C++23, which has
  // more ergonoic span handling.
  zx::result<> SetDisplayLayers(display::DisplayId display_id,
                                const std::vector<LayerConfig>& layer_configs);

  zx::result<> SetLayerImage(display::LayerId layer_id, display::ImageId image_id,
                             display::EventId event_id);
  zx::result<> CheckConfig();
  zx::result<> ApplyConfig();
  zx::result<> AcknowledgeVsync(display::VsyncAckCookie vsync_ack_cookie);
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb);

  zx::result<display::ImageId> ImportImageWithSysmem(const display::ImageMetadata& image_metadata);

  // Imports an image that covers the first connected display.
  //
  // Crashes if no display is connected.
  zx::result<display::ImageId> CreateFullscreenImage();

  // Creates a layer that covers the first connected display.
  //
  // Crashes if no display is connected.
  zx::result<display::LayerId> CreateFullscreenLayer();

  zx::result<EventInfo> CreateEvent();

  // Returns a one-layer configuration that covers the first connected display.
  //
  // Crashes if no display is connected.
  std::vector<LayerConfig> CreateFullscreenLayerConfig();

  // Applies a configuration to the first connected display.
  //
  // Crashes if no display is connected.
  //
  // The std::vector can be converted to std::span once we adopt C++23, which has
  // more ergonoic span handling.
  zx::result<> ApplyLayers(const std::vector<LayerConfig>& layer_configs);

  fuchsia_hardware_display_types::wire::ConfigStamp GetRecentAppliedConfigStamp();

 private:
  display::ImageId next_imported_image_id_{1};

  fidl::WireSyncClient<fuchsia_hardware_display::Coordinator> coordinator_fidl_client_;
  const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem_;

  // Must outlive `coordinator_listener_`.
  TestClientState state_;

  // Must outlive `coordinator_listener_binding_`.
  MockCoordinatorListener coordinator_listener_{
      fit::bind_member<&TestClientState::OnDisplaysChanged>(&state_),
      fit::bind_member<&TestClientState::OnVsync>(&state_),
      fit::bind_member<&TestClientState::OnClientOwnershipChange>(&state_)};
  async_dispatcher_t* coordinator_listener_dispatcher_ = nullptr;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_display::CoordinatorListener>>
      coordinator_listener_binding_;
};

TestFidlClient::TestFidlClient(const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>* sysmem)
    : sysmem_(*sysmem) {
  ZX_ASSERT(sysmem != nullptr);
}

TestFidlClient::~TestFidlClient() {
  if (coordinator_listener_binding_.has_value()) {
    ZX_ASSERT(coordinator_listener_dispatcher_ != nullptr);
    // We can call Unbind() on any thread, but it's async and previously-started dispatches can
    // still be in-flight after this call.
    coordinator_listener_binding_->Unbind();
    // The Unbind() above will prevent starting any new dispatches, but previously-started
    // dispatches can still be in-flight. For this reason we must fence the Bind's dispatcher thread
    // before we delete stuff used during dispatch such as on_vsync_callback_.
    libsync::Completion done;
    zx::result<> post_task_result = display::PostTask<display_coordinator::kDisplayTaskTargetSize>(
        *coordinator_listener_dispatcher_, [&done] { done.Signal(); });
    ZX_ASSERT(post_task_result.is_ok());
    done.Wait();
    // Now it's safe to delete on_vsync_callback_ (for example).
  }
}

zx::result<> TestFidlClient::OpenCoordinator(
    const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
    ClientPriority client_priority, async_dispatcher_t* coordinator_listener_dispatcher) {
  ZX_ASSERT(coordinator_listener_dispatcher != nullptr);
  ZX_ASSERT_MSG(!coordinator_listener_binding_.has_value(), "OpenCoordinator() already called");
  ZX_ASSERT_MSG(coordinator_listener_dispatcher_ == nullptr, "OpenCoordinator() already called");

  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [coordinator_listener_client, coordinator_listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  FDF_LOG(INFO, "Opening coordinator");
  if (client_priority == ClientPriority::kVirtcon) {
    fidl::Arena arena;
    auto request = fidl::WireRequest<fuchsia_hardware_display::Provider::
                                         OpenCoordinatorWithListenerForVirtcon>::Builder(arena)
                       .coordinator(std::move(coordinator_server))
                       .coordinator_listener(std::move(coordinator_listener_client))
                       .Build();
    auto response = provider->OpenCoordinatorWithListenerForVirtcon(std::move(request));
    if (!response.ok()) {
      FDF_LOG(ERROR, "Could not open Virtcon coordinator, error=%s",
              response.FormatDescription().c_str());
      return zx::make_result(response.status());
    }
  } else {
    ZX_DEBUG_ASSERT(client_priority == ClientPriority::kPrimary);
    fidl::Arena arena;
    auto request = fidl::WireRequest<fuchsia_hardware_display::Provider::
                                         OpenCoordinatorWithListenerForPrimary>::Builder(arena)
                       .coordinator(std::move(coordinator_server))
                       .coordinator_listener(std::move(coordinator_listener_client))
                       .Build();
    auto response = provider->OpenCoordinatorWithListenerForPrimary(std::move(request));
    if (!response.ok()) {
      FDF_LOG(ERROR, "Could not open coordinator, error=%s", response.FormatDescription().c_str());
      return zx::make_result(response.status());
    }
  }

  coordinator_fidl_client_.Bind(std::move(coordinator_client));
  coordinator_listener_dispatcher_ = coordinator_listener_dispatcher;
  coordinator_listener_binding_.emplace(fidl::BindServer(coordinator_listener_dispatcher,
                                                         std::move(coordinator_listener_server),
                                                         &coordinator_listener_));
  return zx::ok();
}

zx::result<> TestFidlClient::SetVirtconMode(
    fuchsia_hardware_display::wire::VirtconMode virtcon_mode) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  fidl::OneWayStatus fidl_status = coordinator_fidl_client_->SetVirtconMode(virtcon_mode);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "SetVirtconMode() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::ImportEvent(zx::event event, display::EventId event_id) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fuchsia_hardware_display::wire::EventId fidl_event_id = display::ToFidlEventId(event_id);

  fidl::OneWayStatus fidl_status =
      coordinator_fidl_client_->ImportEvent(std::move(event), fidl_event_id);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "ImportEvent() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<display::LayerId> TestFidlClient::CreateLayer() {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fidl::WireResult<fuchsia_hardware_display::Coordinator::CreateLayer> fidl_status =
      coordinator_fidl_client_->CreateLayer();
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "CreateLayer() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }

  const fit::result<zx_status_t, fuchsia_hardware_display::wire::CoordinatorCreateLayerResponse*>&
      fidl_value = fidl_status.value();
  if (fidl_value.is_error()) {
    FDF_LOG(ERROR, "CreateLayer() returned error: %s",
            zx_status_get_string(fidl_value.error_value()));
    return zx::error(fidl_value.error_value());
  }

  return zx::ok(display::ToLayerId(fidl_value.value()->layer_id));
}

zx::result<> TestFidlClient::ImportImage(const display::ImageMetadata& image_metadata,
                                         display::BufferId image_buffer_id,
                                         display::ImageId image_id) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fuchsia_hardware_display::wire::BufferId fidl_image_buffer_id =
      display::ToFidlBufferId(image_buffer_id);
  const fuchsia_hardware_display::wire::ImageId fidl_image_id = display::ToFidlImageId(image_id);

  const fidl::WireResult<fuchsia_hardware_display::Coordinator::ImportImage> fidl_status =
      coordinator_fidl_client_->ImportImage(image_metadata.ToFidl(), fidl_image_buffer_id,
                                            fidl_image_id);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "ImportImage() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }

  const fit::result<zx_status_t>& fidl_value = fidl_status.value();
  if (fidl_value.is_error()) {
    FDF_LOG(ERROR, "ImportImage() returned error: %s",
            zx_status_get_string(fidl_value.error_value()));
    return zx::error(fidl_value.error_value());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::SetDisplayLayers(display::DisplayId display_id,
                                              const std::vector<LayerConfig>& layer_configs) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  std::vector<fuchsia_hardware_display::wire::LayerId> fidl_layer_ids;
  fidl_layer_ids.reserve(layer_configs.size());
  for (const LayerConfig& layer_config : layer_configs) {
    ZX_ASSERT(layer_config.layer_id != display::kInvalidLayerId);
    const fuchsia_hardware_display::wire::LayerId fidl_layer_id =
        display::ToFidlLayerId(layer_config.layer_id);
    fidl_layer_ids.push_back(fidl_layer_id);
  }

  const fidl::OneWayStatus fidl_status = coordinator_fidl_client_->SetDisplayLayers(
      display::ToFidlDisplayId(display_id),
      fidl::VectorView<fuchsia_hardware_display::wire::LayerId>::FromExternal(fidl_layer_ids));
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "SetDisplayLayers() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::SetLayerImage(display::LayerId layer_id, display::ImageId image_id,
                                           display::EventId event_id) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fuchsia_hardware_display::wire::LayerId fidl_layer_id = display::ToFidlLayerId(layer_id);
  const fuchsia_hardware_display::wire::ImageId fidl_image_id = display::ToFidlImageId(image_id);
  const fuchsia_hardware_display::wire::EventId fidl_event_id = display::ToFidlEventId(event_id);

  const fidl::OneWayStatus fidl_status =
      coordinator_fidl_client_->SetLayerImage2(fidl_layer_id, fidl_image_id, fidl_event_id);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "SetLayerImage2() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::CheckConfig() {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fidl::WireResult<fuchsia_hardware_display::Coordinator::CheckConfig> fidl_status =
      coordinator_fidl_client_->CheckConfig(/*discard=*/false);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "CheckConfig() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  const fidl::WireResponse<fuchsia_hardware_display::Coordinator::CheckConfig>& fidl_result =
      fidl_status.value();
  if (fidl_result.res != fuchsia_hardware_display_types::wire::ConfigResult::kOk) {
    FDF_LOG(ERROR, "CheckConfig() rejected the config: code %" PRIu32,
            static_cast<uint32_t>(fidl_result.res));
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok();
}

zx::result<> TestFidlClient::ApplyConfig() {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fidl::OneWayStatus fidl_status = coordinator_fidl_client_->ApplyConfig();
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "ApplyConfig() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::AcknowledgeVsync(display::VsyncAckCookie vsync_ack_cookie) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fuchsia_hardware_display::wire::VsyncAckCookie fidl_vsync_ack_cookie =
      display::ToFidlVsyncAckCookie(vsync_ack_cookie);
  const fidl::OneWayStatus fidl_status =
      coordinator_fidl_client_->AcknowledgeVsync(fidl_vsync_ack_cookie.value);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "AcknowledgeVsync() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return zx::ok();
}

zx::result<> TestFidlClient::SetMinimumRgb(uint8_t minimum_rgb) {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  const fidl::WireResult<fuchsia_hardware_display::Coordinator::SetMinimumRgb> fidl_status =
      coordinator_fidl_client_->SetMinimumRgb(minimum_rgb);
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "SetMinimumRgb() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }

  const fit::result<zx_status_t>& fidl_value = fidl_status.value();
  if (fidl_value.is_error()) {
    FDF_LOG(ERROR, "SetMinimumRgb() returned error: %s",
            zx_status_get_string(fidl_value.error_value()));
    return zx::error(fidl_value.error_value());
  }
  return zx::ok();
}

zx::result<display::ImageId> TestFidlClient::CreateFullscreenImage() {
  return ImportImageWithSysmem(state_.FullscreenImageMetadata());
}

zx::result<display::LayerId> TestFidlClient::CreateFullscreenLayer() {
  ZX_ASSERT(coordinator_fidl_client_.is_valid());

  zx::result<display::LayerId> layer_id_result = CreateLayer();
  if (layer_id_result.is_error()) {
    // CreateLayer() has already logged the error.
    return layer_id_result;
  }

  fidl::OneWayStatus fidl_status = coordinator_fidl_client_->SetLayerPrimaryConfig(
      display::ToFidlLayerId(layer_id_result.value()), state_.FullscreenImageMetadata().ToFidl());
  if (!fidl_status.ok()) {
    FDF_LOG(ERROR, "SetLayerPrimaryConfig() failed: %s", fidl_status.status_string());
    return zx::error(fidl_status.status());
  }
  return layer_id_result;
}

zx::result<TestFidlClient::EventInfo> TestFidlClient::CreateEvent() {
  zx::event event;
  zx_status_t create_status = zx::event::create(0u, &event);
  if (create_status != ZX_OK) {
    FDF_LOG(ERROR, "zx::event::create() failed: %s", zx_status_get_string(create_status));
    return zx::error(create_status);
  }

  zx_info_handle_basic_t event_handle_info;
  zx_status_t get_info_status = event.get_info(ZX_INFO_HANDLE_BASIC, &event_handle_info,
                                               sizeof(event_handle_info), nullptr, nullptr);
  if (get_info_status != ZX_OK) {
    FDF_LOG(ERROR, "zx::event::get_info() failed: %s", zx_status_get_string(get_info_status));
    return zx::error(get_info_status);
  }

  zx::event event_duplicate;
  zx_status_t duplicate_status = event.duplicate(ZX_RIGHT_SAME_RIGHTS, &event_duplicate);
  if (duplicate_status != ZX_OK) {
    FDF_LOG(ERROR, "zx::event::duplicate() failed: %s", zx_status_get_string(duplicate_status));
    return zx::error(duplicate_status);
  }

  const display::EventId event_id(event_handle_info.koid);
  zx::result<> import_result = ImportEvent(std::move(event), event_id);
  if (import_result.is_error()) {
    // ImportEvent() has already logged the error.
    return import_result.take_error();
  }

  return zx::ok(EventInfo{
      .id = event_id,
      .event = std::move(event_duplicate),
  });
}

zx::result<> TestFidlClient::EnableVsyncEventDelivery() {
  return zx::make_result(coordinator_fidl_client_->SetVsyncEventDelivery(true).status());
}

zx::result<> TestFidlClient::ApplyLayers(const std::vector<LayerConfig>& layer_configs) {
  zx::result<> set_display_layers_result = SetDisplayLayers(state_.display_id(), layer_configs);
  if (set_display_layers_result.is_error()) {
    // SetDisplayLayers() has already logged the error.
    return set_display_layers_result;
  }

  for (const LayerConfig& layer_config : layer_configs) {
    zx::result<> set_layer_image_result =
        SetLayerImage(layer_config.layer_id, layer_config.image_id,
                      layer_config.image_ready_wait_event_id.value_or(display::kInvalidEventId));
    if (set_layer_image_result.is_error()) {
      // SetLayerImage() has already logged the error.
      return set_layer_image_result;
    }
  }

  zx::result<> check_config_result = CheckConfig();
  if (check_config_result.is_error()) {
    // CheckConfig() has already logged the error.
    return check_config_result;
  }

  zx::result<> apply_config_result = ApplyConfig();
  if (apply_config_result.is_error()) {
    // ApplyConfig() has already logged the error.
    return apply_config_result;
  }

  return zx::ok();
}

fuchsia_hardware_display_types::wire::ConfigStamp TestFidlClient::GetRecentAppliedConfigStamp() {
  EXPECT_TRUE(coordinator_fidl_client_);
  auto result = coordinator_fidl_client_->GetLatestAppliedConfigStamp();
  EXPECT_TRUE(result.ok());
  return result.value().stamp;
}

std::vector<TestFidlClient::LayerConfig> TestFidlClient::CreateFullscreenLayerConfig() {
  zx::result<display::LayerId> layer_id_result = CreateFullscreenLayer();
  ZX_ASSERT_MSG(layer_id_result.is_ok(), "%s", layer_id_result.status_string());

  zx::result<display::ImageId> image_id_result =
      ImportImageWithSysmem(state_.FullscreenImageMetadata());
  ZX_ASSERT_MSG(image_id_result.is_ok(), "%s", image_id_result.status_string());

  return {
      {.layer_id = layer_id_result.value(),
       .image_id = image_id_result.value(),
       .image_ready_wait_event_id = std::nullopt},
  };
}

zx::result<display::ImageId> TestFidlClient::ImportImageWithSysmem(
    const display::ImageMetadata& image_metadata) {
  // Create all the tokens.
  fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken> local_token;
  {
    auto [client, server] = fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
    fidl::Arena arena;
    auto allocate_shared_request =
        fuchsia_sysmem2::wire::AllocatorAllocateSharedCollectionRequest::Builder(arena);
    allocate_shared_request.token_request(std::move(server));
    auto result = sysmem_->AllocateSharedCollection(allocate_shared_request.Build());
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to allocate shared collection: %s", result.status_string());
      return zx::error(result.status());
    }
    local_token = fidl::WireSyncClient<fuchsia_sysmem2::BufferCollectionToken>(std::move(client));
    EXPECT_NE(ZX_HANDLE_INVALID, local_token.client_end().channel().get());
  }
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();
  {
    fidl::Arena arena;
    auto duplicate_request =
        fuchsia_sysmem2::wire::BufferCollectionTokenDuplicateRequest::Builder(arena);
    duplicate_request.rights_attenuation_mask(ZX_RIGHT_SAME_RIGHTS);
    duplicate_request.token_request(std::move(server));
    if (auto result = local_token->Duplicate(duplicate_request.Build()); !result.ok()) {
      FDF_LOG(ERROR, "Failed to duplicate token: %s", result.FormatDescription().c_str());
      return zx::error(ZX_ERR_NO_MEMORY);
    }
  }

  // Set display buffer constraints.
  static display::BufferCollectionId next_display_collection_id(0);
  const display::BufferCollectionId display_collection_id = ++next_display_collection_id;
  if (auto result = local_token->Sync(); !result.ok()) {
    FDF_LOG(ERROR, "Failed to sync token %d %s", result.status(),
            result.FormatDescription().c_str());
    return zx::error(result.status());
  }

  const fuchsia_hardware_display::wire::BufferCollectionId fidl_display_collection_id =
      ToFidlBufferCollectionId(display_collection_id);
  const auto result = coordinator_fidl_client_->ImportBufferCollection(fidl_display_collection_id,
                                                                       std::move(client));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to call FIDL ImportBufferCollection %lu (%s)",
            display_collection_id.value(), result.status_string());
    return zx::error(result.status());
  }
  if (result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to import buffer collection %lu (%s)", display_collection_id.value(),
            zx_status_get_string(result.value().error_value()));
    return zx::error(result.value().error_value());
  }

  const fuchsia_hardware_display_types::wire::ImageBufferUsage image_buffer_usage = {
      .tiling_type = image_metadata.tiling_type().ToFidl(),
  };

  const auto set_constraints_result = coordinator_fidl_client_->SetBufferCollectionConstraints(
      fidl_display_collection_id, image_buffer_usage);

  if (!set_constraints_result.ok()) {
    FDF_LOG(ERROR, "Failed to call FIDL SetBufferCollectionConstraints %lu (%s)",
            display_collection_id.value(), set_constraints_result.status_string());
    (void)coordinator_fidl_client_->ReleaseBufferCollection(fidl_display_collection_id);
    return zx::error(set_constraints_result.status());
  }
  if (set_constraints_result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to set buffer collection constraints: %s",
            zx_status_get_string(set_constraints_result.value().error_value()));
    (void)coordinator_fidl_client_->ReleaseBufferCollection(fidl_display_collection_id);
    return zx::error(set_constraints_result.value().error_value());
  }

  // Use the local collection so we can read out the error if allocation
  // fails, and to ensure everything's allocated before trying to import it
  // into another process.
  fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection> sysmem_collection;
  {
    auto [client, server] = fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();
    fidl::Arena arena;
    auto bind_shared_request =
        fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena);
    bind_shared_request.token(local_token.TakeClientEnd());
    bind_shared_request.buffer_collection_request(std::move(server));
    if (auto result = sysmem_->BindSharedCollection(bind_shared_request.Build()); !result.ok()) {
      FDF_LOG(ERROR, "Failed to bind shared collection: %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
    sysmem_collection = fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>(std::move(client));
  }
  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  fidl::Arena arena;
  auto set_name_request = fuchsia_sysmem2::wire::NodeSetNameRequest::Builder(arena);
  set_name_request.priority(10000u);
  set_name_request.name("display-client-unittest");
  (void)sysmem_collection->SetName(set_name_request.Build());
  arena.Reset();
  auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena);
  constraints.min_buffer_count(1);
  constraints.usage(fuchsia_sysmem2::wire::BufferUsage::Builder(arena)
                        .none(fuchsia_sysmem2::wire::kNoneUsage)
                        .Build());
  // We specify min_size_bytes 1 so that something is specifying a minimum size. More typically the
  // display client would specify ImageFormatConstraints that implies a non-zero min_size_bytes.
  constraints.buffer_memory_constraints(
      fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena)
          .min_size_bytes(1)
          .ram_domain_supported(true)
          .Build());
  auto set_constraints_request =
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena);
  set_constraints_request.constraints(constraints.Build());
  zx_status_t status = sysmem_collection->SetConstraints(set_constraints_request.Build()).status();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Unable to set constraints (%d)", status);
    return zx::error(status);
  }
  // Wait for the buffers to be allocated.
  auto info_result = sysmem_collection->WaitForAllBuffersAllocated();
  if (!info_result.ok()) {
    FDF_LOG(ERROR, "Waiting for buffers failed (fidl=%d res=%u)", info_result.status(),
            fidl::ToUnderlying(info_result->error_value()));
    zx_status_t status = info_result.status();
    if (status == ZX_OK) {
      status = sysmem::V1CopyFromV2Error(info_result->error_value());
    }
    return zx::error(status);
  }

  auto& info = info_result.value()->buffer_collection_info();
  if (info.buffers().count() < 1) {
    FDF_LOG(ERROR, "Incorrect buffer collection count %zu", info.buffers().count());
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  const display::ImageId image_id = next_imported_image_id_;
  ++next_imported_image_id_;

  const display::BufferId image_buffer_id{
      .buffer_collection_id = display_collection_id,
      .buffer_index = 0,
  };
  zx::result<> import_image_result = ImportImage(image_metadata, image_buffer_id, image_id);
  if (import_image_result.is_error()) {
    // ImportImage() has already logged the error.
    return import_image_result.take_error();
  }

  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  (void)sysmem_collection->Release();
  return zx::ok(image_id);
}

}  // namespace

class IntegrationTest : public TestBase {
 public:
  // Configures the VSync event delivery check in `IsClientActive()`.
  enum class VsyncCheck : bool {
    // The client may or may not be receiving VSync events.
    kNoCheck = false,

    // The client is receiving VSync events.
    kVsyncEnabled = true,
  };

  // Returns -1 if no display exists with the given ID.
  int64_t DisplayLayerCount(display::DisplayId id) {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    auto displays_it = CoordinatorController()->displays_.find(id);
    if (!displays_it.IsValid()) {
      return -1;
    }
    return int64_t{displays_it->layer_count};
  }

  // Returns null if there is no client connected at `client_priority`.
  static ClientProxy* GetClientProxy(Controller& coordinator_controller,
                                     ClientPriority client_priority)
      __TA_REQUIRES(coordinator_controller.mtx()) {
    switch (client_priority) {
      case ClientPriority::kPrimary:
        return coordinator_controller.primary_client_;
      case ClientPriority::kVirtcon:
        return coordinator_controller.virtcon_client_;
    }
    ZX_DEBUG_ASSERT_MSG(false, "Unimplemtened client priority: %d",
                        static_cast<int>(client_priority));
    return nullptr;
  }

  // True iff `client_priority` is the display coordinator's active client
  // and the client's Vsync delivery configuration matches `vsync_check`.
  bool IsClientActive(ClientPriority client_priority,
                      VsyncCheck vsync_check = VsyncCheck::kVsyncEnabled) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    ClientProxy* client_proxy = GetClientProxy(coordinator_controller, client_priority);
    if (client_proxy == nullptr) {
      return false;
    }
    if (coordinator_controller.active_client_ != client_proxy) {
      return false;
    }

    if (vsync_check == VsyncCheck::kVsyncEnabled) {
      fbl::AutoLock<fbl::Mutex> client_proxy_lock(&client_proxy->mtx_);
      if (!client_proxy->vsync_delivery_enabled_) {
        return false;
      }
    }

    return true;
  }

  display::VsyncAckCookie LastAckedCookie(ClientPriority client_priority) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    ClientProxy* client_proxy = GetClientProxy(coordinator_controller, client_priority);
    ZX_ASSERT(client_proxy != nullptr);

    fbl::AutoLock<fbl::Mutex> client_proxy_lock(&client_proxy->mtx_);
    return client_proxy->handler_.LatestAckedCookie();
  }

  void SendVsyncAfterUnbind(std::unique_ptr<TestFidlClient> client, display::DisplayId display_id) {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    // Resetting the client will *start* client tear down.
    //
    // ~MockCoordinatorListener fences the server-side dispatcher thread (consistent with the
    // threading model of its fidl server binding), but that doesn't sync with the client end
    // (intentionally).
    client.reset();
    ClientProxy* client_ptr = CoordinatorController()->active_client_;
    EXPECT_OK(sync_completion_wait(client_ptr->handler_.fidl_unbound(), zx::sec(1).get()));
    // SetVsyncEventDelivery(false) has not completed here, because we are still
    // holding controller()->mtx()
    client_ptr->OnDisplayVsync(display_id, 0, display::kInvalidConfigStamp);
  }

  bool IsClientConnected(ClientPriority client_priority) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    return GetClientProxy(coordinator_controller, client_priority) != nullptr;
  }

  void SendVsyncFromCoordinatorClientProxy() {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    CoordinatorController()->active_client_->OnDisplayVsync(display::kInvalidDisplayId, 0,
                                                            display::kInvalidConfigStamp);
  }

  void SendVsyncFromDisplayEngine() { FakeDisplayEngine().SendVsync(); }

  std::unique_ptr<TestFidlClient> OpenCoordinatorTestFidlClient(
      const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>* sysmem_client,
      const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& display_provider_client,
      ClientPriority client_priority) {
    ZX_ASSERT(sysmem_client != nullptr);
    ZX_ASSERT(sysmem_client->is_valid());
    ZX_ASSERT(display_provider_client.is_valid());

    auto coordinator_client = std::make_unique<TestFidlClient>(&sysmem_client_);
    zx::result<> open_coordinator_result =
        coordinator_client->OpenCoordinator(display_provider_client, client_priority, dispatcher());
    ZX_ASSERT_MSG(open_coordinator_result.is_ok(), "Failed to open coordinator: %s",
                  open_coordinator_result.status_string());

    bool poll_result = PollUntilOnLoop(
        [&]() { return coordinator_client->state().HasOwnershipAndValidDisplay(); });
    ZX_ASSERT_MSG(poll_result,
                  "Failed to wait until client has ownership of the coordinator "
                  "and has a valid display");

    zx::result<> enable_vsync_result = coordinator_client->EnableVsyncEventDelivery();
    ZX_ASSERT_MSG(enable_vsync_result.is_ok(), "Failed to enable Vsync delivery for client: %s",
                  enable_vsync_result.status_string());

    return coordinator_client;
  }

  // |TestBase|
  void SetUp() override {
    TestBase::SetUp();
    auto sysmem = fidl::SyncClient(ConnectToSysmemAllocatorV2());
    EXPECT_TRUE(sysmem.is_valid());
    fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest request;
    request.name() = fsl::GetCurrentProcessName();
    request.id() = fsl::GetCurrentProcessKoid();
    auto set_debug_result = sysmem->SetDebugClientInfo(std::move(request));
    EXPECT_TRUE(set_debug_result.is_ok());
    sysmem_client_ = fidl::WireSyncClient<fuchsia_sysmem2::Allocator>(sysmem.TakeClientEnd());
  }

  // |TestBase|
  void TearDown() override {
    // Wait until the display core has processed all client disconnections before sending the last
    // vsync.
    EXPECT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kPrimary); }));
    EXPECT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kVirtcon); }));

    // Send one last vsync, to make sure any blank configs take effect.
    SendVsyncFromDisplayEngine();
    EXPECT_EQ(0u, CoordinatorController()->ImportedImagesCountForTesting());
    TestBase::TearDown();
  }

 protected:
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_client_;
};

TEST_F(IntegrationTest, DISABLED_ClientsCanBail) {
  for (size_t i = 0; i < 100; i++) {
    ASSERT_TRUE(PollUntilOnLoop([&]() { return !IsClientActive(ClientPriority::kPrimary); }));

    std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
        &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  }
}

TEST_F(IntegrationTest, MustUseUniqueEventIDs) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  zx::event event_a, event_b, event_c;
  ASSERT_OK(zx::event::create(0, &event_a));
  ASSERT_OK(zx::event::create(0, &event_b));
  ASSERT_OK(zx::event::create(0, &event_c));
  {
    static constexpr display::EventId kEventId(123);
    ASSERT_OK(client->ImportEvent(std::move(event_a), kEventId));
    ASSERT_OK(client->ImportEvent(std::move(event_b), kEventId));
    // This test passes if it closes without deadlocking.
  }
  // TODO: Use LLCPP epitaphs when available to detect ZX_ERR_PEER_CLOSED.
}

TEST_F(IntegrationTest, SendVsyncsAfterEmptyConfig) {
  TestFidlClient virtcon_client(&sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    ASSERT_OK(virtcon_client.SetDisplayLayers(virtcon_display_id, {}));
    ASSERT_OK(virtcon_client.ApplyConfig());
  }

  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Present an image
  ASSERT_OK(primary_client->ApplyLayers(primary_client->CreateFullscreenLayerConfig()));
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return DisplayLayerCount(primary_client->state().display_id()) == 1; }));
  uint64_t count = primary_client->state().vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->state().vsync_count() > count; }));

  // Set an empty config
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  display::ConfigStamp empty_config_stamp = CoordinatorController()->TEST_controller_stamp();
  // Wait for it to apply
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return DisplayLayerCount(primary_client->state().display_id()) == 0; }));

  // The old client disconnects
  primary_client.reset();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kPrimary); }));

  // A new client connects
  primary_client = OpenCoordinatorTestFidlClient(&sysmem_client_, DisplayProviderClient(),
                                                 ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  // ... and presents before the previous client's empty vsync
  ASSERT_OK(primary_client->ApplyLayers(primary_client->CreateFullscreenLayerConfig()));
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return DisplayLayerCount(primary_client->state().display_id()) == 1; }));

  // Empty vsync for last client. Nothing should be sent to the new client.
  const config_stamp_t banjo_config_stamp = ToBanjoConfigStamp(empty_config_stamp);
  CoordinatorController()->DisplayEngineListenerOnDisplayVsync(
      ToBanjoDisplayId(primary_client->state().display_id()), 0u, &banjo_config_stamp);

  // Send a second vsync, using the config the client applied.
  count = primary_client->state().vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->state().vsync_count() > count; }));
}

TEST_F(IntegrationTest, DISABLED_SendVsyncsAfterClientsBail) {
  TestFidlClient virtcon_client(&sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    ASSERT_OK(virtcon_client.SetDisplayLayers(virtcon_display_id, {}));
    ASSERT_OK(virtcon_client.ApplyConfig());
  }

  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Present an image
  ASSERT_OK(primary_client->ApplyLayers(primary_client->CreateFullscreenLayerConfig()));
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return DisplayLayerCount(primary_client->state().display_id()) == 1; }));

  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->state().vsync_count() >= 1; }));
  // Send the controller a vsync for an image / a config it won't recognize anymore.
  display::ConfigStamp invalid_config_stamp =
      CoordinatorController()->TEST_controller_stamp() - display::ConfigStamp{1};
  const config_stamp_t invalid_banjo_config_stamp = ToBanjoConfigStamp(invalid_config_stamp);
  CoordinatorController()->DisplayEngineListenerOnDisplayVsync(
      ToBanjoDisplayId(primary_client->state().display_id()), 0u, &invalid_banjo_config_stamp);

  // Send a second vsync, using the config the client applied.
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->state().vsync_count() >= 2; }));
  EXPECT_EQ(2u, primary_client->state().vsync_count());
}

TEST_F(IntegrationTest, SendVsyncsAfterClientDies) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  display::DisplayId display_id = primary_client->state().display_id();
  SendVsyncAfterUnbind(std::move(primary_client), display_id);
}

TEST_F(IntegrationTest, AcknowledgeVsync) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  EXPECT_EQ(0u, primary_client->state().vsync_count());

  // send vsyncs up to watermark level
  for (uint32_t i = 0; i < ClientProxy::kVsyncMessagesWatermark; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return primary_client->state().last_vsync_ack_cookie() != display::kInvalidVsyncAckCookie;
  }));
  EXPECT_EQ(ClientProxy::kVsyncMessagesWatermark, primary_client->state().vsync_count());

  // acknowledge
  ASSERT_OK(primary_client->AcknowledgeVsync(primary_client->state().last_vsync_ack_cookie()));
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return LastAckedCookie(ClientPriority::kPrimary) ==
           primary_client->state().last_vsync_ack_cookie();
  }));
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterQueueFull) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  uint32_t vsync_count = ClientProxy::kMaxVsyncMessages;
  while (vsync_count--) {
    SendVsyncFromCoordinatorClientProxy();
  }
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return (primary_client->state().vsync_count() >= expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->state().last_vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());

  // now let's acknowledge vsync
  ASSERT_OK(primary_client->AcknowledgeVsync(primary_client->state().last_vsync_ack_cookie()));
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return LastAckedCookie(ClientPriority::kPrimary) ==
           primary_client->state().last_vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages + kNumVsync + 1;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() >= expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterLongTime) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsyncs
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return primary_client->state().vsync_count() >= ClientProxy::kMaxVsyncMessages; }));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->state().last_vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a lot
  constexpr uint32_t kNumVsync = ClientProxy::kVsyncBufferSize * 10;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());

  // now let's acknowledge vsync
  ASSERT_OK(primary_client->AcknowledgeVsync(primary_client->state().last_vsync_ack_cookie()));
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return LastAckedCookie(ClientPriority::kPrimary) ==
           primary_client->state().last_vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count =
        ClientProxy::kMaxVsyncMessages + ClientProxy::kVsyncBufferSize + 1;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() >= expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
}

TEST_F(IntegrationTest, InvalidVSyncCookie) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return (primary_client->state().vsync_count() >= ClientProxy::kMaxVsyncMessages); }));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->state().last_vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());

  // now let's acknowledge vsync with invalid cookie
  static constexpr display::VsyncAckCookie kInvalidCookie(0xdeadbeef);
  ASSERT_NE(primary_client->state().last_vsync_ack_cookie(), kInvalidCookie);
  ASSERT_OK(primary_client->AcknowledgeVsync(kInvalidCookie));

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return LastAckedCookie(ClientPriority::kPrimary) ==
             primary_client->state().last_vsync_ack_cookie();
    });
  }
  EXPECT_NE(LastAckedCookie(ClientPriority::kPrimary),
            primary_client->state().last_vsync_ack_cookie());

  // We should still not receive vsync events since acknowledge did not use valid cookie
  SendVsyncFromCoordinatorClientProxy();
  constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return primary_client->state().vsync_count() >= expected_vsync_count + 1;
    });
  }
  EXPECT_LT(primary_client->state().vsync_count(), expected_vsync_count + 1);

  EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
}

TEST_F(IntegrationTest, AcknowledgeVsyncWithOldCookie) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() >= expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->state().last_vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->state().vsync_count());

  // now let's acknowledge vsync

  ASSERT_OK(primary_client->AcknowledgeVsync(primary_client->state().last_vsync_ack_cookie()));
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return LastAckedCookie(ClientPriority::kPrimary) ==
           primary_client->state().last_vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages + kNumVsync + 1;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return (primary_client->state().vsync_count() >= expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }

  // save old cookie
  display::VsyncAckCookie old_vsync_ack_cookie = primary_client->state().last_vsync_ack_cookie();

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }

  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages * 2;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return (primary_client->state().vsync_count() >= expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->state().last_vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  for (uint32_t i = 0; i < ClientProxy::kVsyncBufferSize; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages * 2, primary_client->state().vsync_count());

  // now let's acknowledge vsync with old cookie
  ASSERT_OK(primary_client->AcknowledgeVsync(old_vsync_ack_cookie));

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return LastAckedCookie(ClientPriority::kPrimary) ==
             primary_client->state().last_vsync_ack_cookie();
    });
  }
  EXPECT_NE(LastAckedCookie(ClientPriority::kPrimary),
            primary_client->state().last_vsync_ack_cookie());

  // Since we did not acknowledge with most recent cookie, we should not get any vsync events back
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages * 2;

    // This check can have a false positive pass, due to using a hard-coded
    // timeout.
    {
      zx::time deadline = zx::deadline_after(zx::sec(1));
      PollUntilOnLoop([&]() {
        if (zx::clock::get_monotonic() >= deadline)
          return true;
        return primary_client->state().vsync_count() >= expected_vsync_count + 1;
      });
    }
    EXPECT_LT(primary_client->state().vsync_count(), expected_vsync_count + 1);

    // count should still remain the same
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }

  // now let's acknowledge with valid cookie
  ASSERT_OK(primary_client->AcknowledgeVsync(primary_client->state().last_vsync_ack_cookie()));
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return LastAckedCookie(ClientPriority::kPrimary) ==
           primary_client->state().last_vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count =
        ClientProxy::kMaxVsyncMessages * 2 + ClientProxy::kVsyncBufferSize + 1;
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() >= expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->state().vsync_count());
  }
}

TEST_F(IntegrationTest, CreateLayer) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  EXPECT_OK(client->CreateFullscreenLayer());
}

TEST_F(IntegrationTest, ImportImageWithInvalidImageId) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  constexpr display::ImageId image_id = display::kInvalidImageId;
  constexpr display::BufferCollectionId buffer_collection_id(0xffeeeedd);

  zx::result<> import_image_result = client->ImportImage(
      client->state().FullscreenImageMetadata(),
      display::BufferId{.buffer_collection_id = buffer_collection_id, .buffer_index = 0}, image_id);
  EXPECT_NE(ZX_OK, import_image_result.status_value()) << import_image_result.status_string();
}

TEST_F(IntegrationTest, ImportImageWithNonExistentBufferCollectionId) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  constexpr display::BufferCollectionId kNonExistentCollectionId(0xffeeeedd);
  constexpr display::ImageId image_id(1);
  zx::result<> import_image_result = client->ImportImage(
      client->state().FullscreenImageMetadata(),
      display::BufferId{.buffer_collection_id = kNonExistentCollectionId, .buffer_index = 0},
      image_id);
  EXPECT_NE(ZX_OK, import_image_result.status_value()) << import_image_result.status_string();
}

TEST_F(IntegrationTest, ClampRgb) {
  TestFidlClient virtcon_client(&sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    // set mode to Fallback
    ASSERT_OK(virtcon_client.SetVirtconMode(fuchsia_hardware_display::VirtconMode::kFallback));
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));
    // Clamp RGB to a minimum value
    ASSERT_OK(virtcon_client.SetMinimumRgb(32));
    ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 32; }));
  }

  // Create a primary client
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  // Clamp RGB to a new value
  ASSERT_OK(primary_client->SetMinimumRgb(1));
  ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 1; }));
  // close client and wait for virtcon to become active again
  primary_client.reset(nullptr);
  // Apply a config for virtcon client to become active.
  {
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    ASSERT_OK(virtcon_client.SetDisplayLayers(virtcon_display_id, {}));
    ASSERT_OK(virtcon_client.ApplyConfig());
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));
  SendVsyncFromDisplayEngine();
  // make sure clamp value was restored
  ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 32; }));
}

// TODO(https://fxbug.dev/340926351): De-flake and reenable this test.
TEST_F(IntegrationTest, DISABLED_EmptyConfigIsNotApplied) {
  // Create and bind virtcon client.
  TestFidlClient virtcon_client(&sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  ASSERT_OK(virtcon_client.SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode::kFallback));
  {
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    ASSERT_OK(virtcon_client.SetDisplayLayers(virtcon_display_id, {}));
    ASSERT_OK(virtcon_client.ApplyConfig());
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));

  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Virtcon client should remain active until primary client has set a config.
  uint64_t virtcon_client_vsync_count = virtcon_client.state().vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return virtcon_client.state().vsync_count() > virtcon_client_vsync_count; }));
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->state().vsync_count() == 0; }));

  // Present an image from the primary client.
  ASSERT_OK(primary_client->ApplyLayers(primary_client->CreateFullscreenLayerConfig()));
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return DisplayLayerCount(primary_client->state().display_id()) == 1; }));

  // Primary client should have become active after a config was set.
  const uint64_t primary_vsync_count = primary_client->state().vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
}

// This tests the basic behavior of ApplyConfig() and OnVsync() events.
// We test applying configurations with images without wait fences, so they are
// guaranteed to be ready when client calls ApplyConfig().
//
// In this case, the new configuration stamp is guaranteed to appear in the next
// coming OnVsync() event.
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_2
//  * ApplyConfig({}) ==> config_stamp_3
//  - Vsync now should have config_stamp_3
//
// Both images are ready at ApplyConfig() time, i.e. no fences are provided.
TEST_F(IntegrationTest, VsyncEvent) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }

  display::ConfigStamp present_config_stamp_0 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result =
      primary_client->CreateFullscreenLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateFullscreenImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateFullscreenImage();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();

  // Present one single image without wait.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_1 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer without wait.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_2 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_2, present_config_stamp_2);

  // Hide the existing layer.
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  auto apply_config_stamp_3 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_3);
  EXPECT_GT(apply_config_stamp_3, apply_config_stamp_2);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(0, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_3 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_3, present_config_stamp_3);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// come with wait fences, which is a common use case in Scenic when using GPU
// composition.
//
// When applying configurations with pending images, the config_stamp returned
// from OnVsync() should not be updated unless the image becomes ready and
// triggers a ReapplyConfig().
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, wait on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1
//  * Signal fence1
//  - Vsync now should have config_stamp_2
//
TEST_F(IntegrationTest, VsyncWaitForPendingImages) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }

  display::ConfigStamp present_config_stamp_0 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result =
      primary_client->CreateFullscreenLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateFullscreenImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateFullscreenImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_1_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());

  // Present one single image without wait.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  auto present_config_stamp_1 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer; but the image is not ready yet. So the
  // configuration applied to display device will be still the old one. On Vsync
  // the |presented_config_stamp| is still |config_stamp_1|.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = std::make_optional(image_1_ready_fence.id)},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GE(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_2 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Signal the event. Display Fence callback will be signaled, and new
  // configuration with new config stamp (config_stamp_2) will be used.
  // On next Vsync, the |presented_config_stamp| will be updated.
  auto old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_1_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);
  ASSERT_TRUE(PollUntilOnLoop([controller = CoordinatorController(), old_controller_stamp]() {
    return controller->TEST_controller_stamp() > old_controller_stamp;
  }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_3 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_2);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// that comes with wait fences are hidden in subsequent configurations.
//
// If a pending image never becomes ready, the config_stamp returned from
// OnVsync() should not be updated unless the image layer has been removed from
// the display in a subsequent configuration.
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, waiting on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({}) ==> config_stamp_3
//  - Vsync now should have config_stamp_3
//
// Note that fence1 is never signaled.
//
TEST_F(IntegrationTest, VsyncHidePendingLayer) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }

  display::ConfigStamp present_config_stamp_0 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result =
      primary_client->CreateFullscreenLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateFullscreenImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateFullscreenImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_1_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());

  // Present an image layer.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  auto present_config_stamp_1 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer; but the image is not ready yet. Display
  // controller will wait on the fence and Vsync will return the previous
  // configuration instead.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = image_1_ready_fence.id},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_2 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Hide the image layer. Display controller will not care about the fence
  // and thus use the latest configuration stamp.
  {
    ASSERT_OK(primary_client->SetDisplayLayers(primary_client->state().display_id(), {}));
    ASSERT_OK(primary_client->ApplyConfig());
  }
  auto apply_config_stamp_3 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_3);
  EXPECT_GE(apply_config_stamp_3, apply_config_stamp_2);

  // On Vsync, the configuration stamp client receives on Vsync event message
  // will be the latest one applied to the display controller, since the pending
  // image has been removed from the configuration.
  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(0, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_3 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_3);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// that comes with wait fences are overridden in subsequent configurations.
//
// If a client applies a configuration (#1) with a pending image, while display
// controller waits for the image to be ready, the client may apply another
// configuration (#2) with a different image. If the image in configuration #2
// becomes available earlier than #1, the layer configuration in #1 should be
// overridden, and signaling wait fences in #1 should not trigger a
// ReapplyConfig().
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, waiting on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1 since img1 is not ready yet
//  * ApplyConfig({layerA: img2, waiting on fence2}) ==> config_stamp_3
//  - Vsync now should have config_stamp_1 since img1 and img2 are not ready
//  * Signal fence2
//  - Vsync now should have config_stamp_3.
//  * Signal fence1
//  - Vsync .
//
// Note that fence1 is never signaled.
TEST_F(IntegrationTest, VsyncSkipOldPendingConfiguration) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      &sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  zx::result<display::LayerId> create_default_layer_result =
      primary_client->CreateFullscreenLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateFullscreenImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateFullscreenImage();
  zx::result<display::ImageId> create_image_2_result = primary_client->CreateFullscreenImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();
  zx::result<TestFidlClient::EventInfo> create_image_2_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_2_result);
  EXPECT_OK(create_image_1_ready_fence_result);
  EXPECT_OK(create_image_2_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  display::ImageId image_2_id = create_image_2_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());
  TestFidlClient::EventInfo image_2_ready_fence =
      std::move(create_image_2_ready_fence_result.value());

  // Apply a config for client to become active; Present an image layer.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_0 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  // Present another image layer (image #1, wait_event #0); but the image is not
  // ready yet. Display controller will wait on the fence and Vsync will return
  // the previous configuration instead.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = image_1_ready_fence.id},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }

  display::ConfigStamp present_config_stamp_1 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_1, present_config_stamp_0);

  // Present another image layer (image #2, wait_event #1); the image is not
  // ready as well. We should still see current |presented_config_stamp| to be
  // equal to |present_config_stamp_0|.
  ASSERT_OK(primary_client->ApplyLayers({
      {.layer_id = default_layer_id,
       .image_id = image_2_id,
       .image_ready_wait_event_id = image_2_ready_fence.id},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }

  display::ConfigStamp present_config_stamp_2 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Signal the event #1. Display Fence callback will be signaled, and
  // configuration with new config stamp (apply_config_stamp_2) will be used.
  // On next Vsync, the |presented_config_stamp| will be updated.
  auto old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_2_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return CoordinatorController()->TEST_controller_stamp() > old_controller_stamp; }));

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_3 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_2);

  // Signal the event #0. Since we have displayed a newer image, signaling the
  // old event associated with the old image shouldn't trigger ReapplyConfig().
  // We should still see |apply_config_stamp_2| as the latest presented config
  // stamp in the client.
  old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_1_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);

  {
    const uint64_t primary_vsync_count = primary_client->state().vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return primary_client->state().vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->state().display_id()));

  display::ConfigStamp present_config_stamp_4 = primary_client->state().last_vsync_config_stamp();
  EXPECT_EQ(present_config_stamp_4, apply_config_stamp_2);
}

// TODO(https://fxbug.dev/42171874): Currently the fake-display driver only supports one
// primary layer. In order to better test ApplyConfig() / OnVsync() behavior,
// we should make fake-display driver support multi-layer configurations and
// then we could add more multi-layer tests.

}  // namespace display_coordinator
