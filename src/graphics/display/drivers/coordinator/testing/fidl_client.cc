// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/testing/fidl_client.h"

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

TestFidlClient::Display::Display(const fuchsia_hardware_display::wire::Info& info) {
  id_ = display::ToDisplayId(info.id);

  for (size_t i = 0; i < info.pixel_format.count(); i++) {
    pixel_formats_.push_back(info.pixel_format[i]);
  }
  for (size_t i = 0; i < info.modes.count(); i++) {
    modes_.push_back(info.modes[i]);
  }
  manufacturer_name_ = fbl::String(info.manufacturer_name.data());
  monitor_name_ = fbl::String(info.monitor_name.data());
  monitor_serial_ = fbl::String(info.monitor_serial.data());
  image_metadata_ = {
      .dimensions = {.width = modes_[0].active_area.width, .height = modes_[0].active_area.height},
      .tiling_type = fuchsia_hardware_display_types::wire::kImageTilingTypeLinear,
  };
}

display::DisplayId TestFidlClient::display_id() const { return displays_[0].id_; }

zx::result<> TestFidlClient::OpenCoordinator(
    const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
    ClientPriority client_priority, async_dispatcher_t* coordinator_listener_dispatcher) {
  ZX_ASSERT(coordinator_listener_dispatcher != nullptr);

  auto [dc_client, dc_server] = fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [coordinator_listener_client, coordinator_listener_server] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();
  FDF_LOG(INFO, "Opening coordinator");
  if (client_priority == ClientPriority::kVirtcon) {
    fidl::Arena arena;
    auto request = fidl::WireRequest<fuchsia_hardware_display::Provider::
                                         OpenCoordinatorWithListenerForVirtcon>::Builder(arena)
                       .coordinator(std::move(dc_server))
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
                       .coordinator(std::move(dc_server))
                       .coordinator_listener(std::move(coordinator_listener_client))
                       .Build();
    auto response = provider->OpenCoordinatorWithListenerForPrimary(std::move(request));
    if (!response.ok()) {
      FDF_LOG(ERROR, "Could not open coordinator, error=%s", response.FormatDescription().c_str());
      return zx::make_result(response.status());
    }
  }

  fbl::AutoLock lock(mtx());
  dc_.Bind(std::move(dc_client));
  coordinator_listener_.Bind(std::move(coordinator_listener_server),
                             coordinator_listener_dispatcher);
  return zx::ok();
}

zx::result<display::ImageId> TestFidlClient::CreateImage() {
  return ImportImageWithSysmem(displays_[0].image_metadata_);
}

zx::result<display::LayerId> TestFidlClient::CreateLayer() {
  fbl::AutoLock lock(mtx());
  return CreateLayerLocked();
}

zx::result<TestFidlClient::EventInfo> TestFidlClient::CreateEvent() {
  fbl::AutoLock lock(mtx());
  return CreateEventLocked();
}

zx::result<display::LayerId> TestFidlClient::CreateLayerLocked() {
  ZX_DEBUG_ASSERT(dc_);
  auto reply = dc_->CreateLayer();
  if (!reply.ok()) {
    FDF_LOG(ERROR, "Failed to create layer (fidl=%d)", reply.status());
    return zx::error(reply.status());
  } else if (reply.value().is_error() != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create layer: %s", zx_status_get_string(reply.value().error_value()));
    return zx::error(reply.value().error_value());
  }
  EXPECT_EQ(
      dc_->SetLayerPrimaryConfig(reply.value()->layer_id, displays_[0].image_metadata_).status(),
      ZX_OK);
  return zx::ok(display::ToLayerId(reply.value()->layer_id));
}

zx::result<TestFidlClient::EventInfo> TestFidlClient::CreateEventLocked() {
  zx::event event;
  if (auto status = zx::event::create(0u, &event); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create zx::event: %d", status);
    return zx::error(status);
  }

  zx_info_handle_basic_t info;
  if (auto status = event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to get zx handle (%u) info: %d", event.get(), status);
    return zx::error(status);
  }

  zx::event dup;
  if (auto status = event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to duplicate zx event (%u): %d", event.get(), status);
    return zx::error(status);
  }

  const display::EventId event_id(info.koid);
  auto import_result = dc_->ImportEvent(std::move(event), ToFidlEventId(event_id));
  if (!import_result.ok()) {
    FDF_LOG(ERROR, "Failed to import event to display controller: %d", import_result.status());
  }

  return zx::ok(EventInfo{
      .id = event_id,
      .event = std::move(dup),
  });
}

bool TestFidlClient::HasOwnershipAndValidDisplay() const {
  return has_ownership_ && !displays_.is_empty();
}

zx::result<> TestFidlClient::EnableVsyncEventDelivery() {
  fbl::AutoLock lock(mtx());
  return zx::make_result(dc_->SetVsyncEventDelivery(true).status());
}

TestFidlClient::~TestFidlClient() = default;

zx_status_t TestFidlClient::PresentLayers(std::vector<PresentLayerInfo> present_layers) {
  fbl::AutoLock l(mtx());

  std::vector<fuchsia_hardware_display::wire::LayerId> fidl_layers;
  for (const auto& info : present_layers) {
    fidl_layers.push_back(ToFidlLayerId(info.layer_id));
  }
  if (auto reply = dc_->SetDisplayLayers(
          ToFidlDisplayId(display_id()),
          fidl::VectorView<fuchsia_hardware_display::wire::LayerId>::FromExternal(fidl_layers));
      !reply.ok()) {
    return reply.status();
  }

  for (const auto& info : present_layers) {
    const fuchsia_hardware_display::wire::LayerId fidl_layer_id = ToFidlLayerId(info.layer_id);
    const display::EventId wait_event_id =
        info.image_ready_wait_event_id.value_or(display::kInvalidEventId);
    if (auto reply = dc_->SetLayerImage2(fidl_layer_id, ToFidlImageId(info.image_id),
                                         /*wait_event_id=*/ToFidlEventId(wait_event_id));
        !reply.ok()) {
      return reply.status();
    }
  }

  if (auto reply = dc_->CheckConfig(false);
      !reply.ok() || reply.value().res != fuchsia_hardware_display_types::wire::ConfigResult::kOk) {
    return reply.ok() ? ZX_ERR_INVALID_ARGS : reply.status();
  }
  return dc_->ApplyConfig().status();
}

fuchsia_hardware_display_types::wire::ConfigStamp TestFidlClient::GetRecentAppliedConfigStamp() {
  fbl::AutoLock lock(mtx());
  EXPECT_TRUE(dc_);
  auto result = dc_->GetLatestAppliedConfigStamp();
  EXPECT_TRUE(result.ok());
  return result.value().stamp;
}

zx::result<display::ImageId> TestFidlClient::ImportImageWithSysmem(
    const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) {
  fbl::AutoLock lock(mtx());
  return ImportImageWithSysmemLocked(image_metadata);
}

std::vector<TestFidlClient::PresentLayerInfo> TestFidlClient::CreateDefaultPresentLayerInfo() {
  zx::result<display::LayerId> layer_result = CreateLayer();
  EXPECT_OK(layer_result);

  zx::result<display::ImageId> image_result = ImportImageWithSysmem(displays_[0].image_metadata_);
  EXPECT_OK(image_result);

  return {
      {.layer_id = layer_result.value(),
       .image_id = image_result.value(),
       .image_ready_wait_event_id = std::nullopt},
  };
}

zx::result<display::ImageId> TestFidlClient::ImportImageWithSysmemLocked(
    const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) {
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
  const auto result = dc_->ImportBufferCollection(fidl_display_collection_id, std::move(client));
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
      .tiling_type = image_metadata.tiling_type,
  };

  const auto set_constraints_result =
      dc_->SetBufferCollectionConstraints(fidl_display_collection_id, image_buffer_usage);

  if (!set_constraints_result.ok()) {
    FDF_LOG(ERROR, "Failed to call FIDL SetBufferCollectionConstraints %lu (%s)",
            display_collection_id.value(), set_constraints_result.status_string());
    (void)dc_->ReleaseBufferCollection(fidl_display_collection_id);
    return zx::error(set_constraints_result.status());
  }
  if (set_constraints_result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to set buffer collection constraints: %s",
            zx_status_get_string(set_constraints_result.value().error_value()));
    (void)dc_->ReleaseBufferCollection(fidl_display_collection_id);
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
  // We specify min_size_bytes 1 so that something is specifying a minimum size.  More typically the
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

  const display::ImageId image_id = next_image_id_++;
  const fuchsia_hardware_display::wire::ImageId fidl_image_id = ToFidlImageId(image_id);
  const auto import_result =
      dc_->ImportImage(image_metadata,
                       fuchsia_hardware_display::wire::BufferId{
                           .buffer_collection_id = fidl_display_collection_id,
                           .buffer_index = 0,
                       },
                       fidl_image_id);
  if (!import_result.ok()) {
    FDF_LOG(ERROR, "Failed to call FIDL ImportImage %" PRIu64 " (%s)", fidl_image_id.value,
            import_result.status_string());
    return zx::error(import_result.status());
  }
  if (import_result.value().is_error()) {
    FDF_LOG(ERROR, "Failed to import image %" PRIu64 " (%s)", fidl_image_id.value,
            zx_status_get_string(import_result.value().error_value()));
    return zx::error(import_result.value().error_value());
  }

  // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
  (void)sysmem_collection->Release();
  return zx::ok(image_id);
}

void TestFidlClient::OnDisplaysChanged(
    std::vector<fuchsia_hardware_display::wire::Info> added_displays,
    std::vector<display::DisplayId> removed_display_ids) {
  for (const fuchsia_hardware_display::wire::Info& added_display : added_displays) {
    displays_.push_back(Display(added_display));
  }
}

void TestFidlClient::OnClientOwnershipChange(bool has_ownership) { has_ownership_ = has_ownership; }

void TestFidlClient::OnVsync(display::DisplayId display_id, zx::time timestamp,
                             display::ConfigStamp applied_config_stamp,
                             display::VsyncAckCookie vsync_ack_cookie) {
  fbl::AutoLock lock(mtx());
  vsync_count_++;
  recent_presented_config_stamp_ = applied_config_stamp;
  if (vsync_ack_cookie != display::kInvalidVsyncAckCookie) {
    vsync_ack_cookie_ = vsync_ack_cookie;
  }
}

}  // namespace display_coordinator
