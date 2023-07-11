// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/tests/fidl_client.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <zircon/assert.h>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/event-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-id.h"
#include "src/graphics/display/lib/api-types-cpp/layer-id.h"
#include "src/graphics/display/lib/api-types-cpp/vsync-ack-cookie.h"
#include "src/lib/testing/predicates/status.h"

namespace fhd = fuchsia_hardware_display;
namespace sysmem = fuchsia_sysmem;

namespace display {

TestFidlClient::Display::Display(const fhd::wire::Info& info) {
  id_ = ToDisplayId(info.id);

  for (size_t i = 0; i < info.pixel_format.count(); i++) {
    pixel_formats_.push_back(info.pixel_format[i]);
  }
  for (size_t i = 0; i < info.modes.count(); i++) {
    modes_.push_back(info.modes[i]);
  }
  for (size_t i = 0; i < info.cursor_configs.count(); i++) {
    cursors_.push_back(info.cursor_configs[i]);
  }
  manufacturer_name_ = fbl::String(info.manufacturer_name.data());
  monitor_name_ = fbl::String(info.monitor_name.data());
  monitor_serial_ = fbl::String(info.monitor_serial.data());
  image_config_.height = modes_[0].vertical_resolution;
  image_config_.width = modes_[0].horizontal_resolution;
  image_config_.type = fhd::wire::kTypeSimple;
}

DisplayId TestFidlClient::display_id() const { return displays_[0].id_; }

bool TestFidlClient::CreateChannel(const fidl::WireSyncClient<fhd::Provider>& provider,
                                   bool is_vc) {
  zx::result endpoints = fidl::CreateEndpoints<fhd::Coordinator>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Could not create coordinator channels, error=%s", endpoints.status_string());
    return false;
  }
  auto& [dc_client, dc_server] = endpoints.value();
  zxlogf(INFO, "Opening coordinator");
  if (is_vc) {
    auto response = provider->OpenCoordinatorForVirtcon(std::move(dc_server));
    if (!response.ok()) {
      zxlogf(ERROR, "Could not open Virtcon coordinator, error=%s",
             response.FormatDescription().c_str());
      return false;
    }
  } else {
    auto response = provider->OpenCoordinatorForPrimary(std::move(dc_server));
    if (!response.ok()) {
      zxlogf(ERROR, "Could not open coordinator, error=%s", response.FormatDescription().c_str());
      return false;
    }
  }

  fbl::AutoLock lock(mtx());
  dc_.Bind(std::move(dc_client));
  return true;
}

zx::result<ImageId> TestFidlClient::CreateImage() {
  return ImportImageWithSysmem(displays_[0].image_config_);
}

zx::result<LayerId> TestFidlClient::CreateLayer() {
  fbl::AutoLock lock(mtx());
  return CreateLayerLocked();
}

zx::result<TestFidlClient::EventInfo> TestFidlClient::CreateEvent() {
  fbl::AutoLock lock(mtx());
  return CreateEventLocked();
}

zx::result<LayerId> TestFidlClient::CreateLayerLocked() {
  ZX_DEBUG_ASSERT(dc_);
  auto reply = dc_->CreateLayer();
  if (!reply.ok()) {
    zxlogf(ERROR, "Failed to create layer (fidl=%d)", reply.status());
    return zx::error(reply.status());
  } else if (reply.value().res != ZX_OK) {
    zxlogf(ERROR, "Failed to create layer (res=%d)", reply.value().res);
    return zx::error(reply.value().res);
  }
  EXPECT_EQ(dc_->SetLayerPrimaryConfig(reply.value().layer_id, displays_[0].image_config_).status(),
            ZX_OK);
  return zx::ok(ToLayerId(reply.value().layer_id));
}

zx::result<TestFidlClient::EventInfo> TestFidlClient::CreateEventLocked() {
  zx::event event;
  if (auto status = zx::event::create(0u, &event); status != ZX_OK) {
    zxlogf(ERROR, "Failed to create zx::event: %d", status);
    return zx::error(status);
  }

  zx_info_handle_basic_t info;
  if (auto status = event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    zxlogf(ERROR, "Failed to get zx handle (%u) info: %d", event.get(), status);
    return zx::error(status);
  }

  zx::event dup;
  if (auto status = event.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup); status != ZX_OK) {
    zxlogf(ERROR, "Failed to duplicate zx event (%u): %d", event.get(), status);
    return zx::error(status);
  }

  const EventId event_id(info.koid);
  auto import_result = dc_->ImportEvent(std::move(event), ToFidlEventId(event_id));
  if (!import_result.ok()) {
    zxlogf(ERROR, "Failed to import event to display controller: %d", import_result.status());
  }

  return zx::ok(EventInfo{
      .id = event_id,
      .event = std::move(dup),
  });
}

bool TestFidlClient::Bind(async_dispatcher_t* dispatcher) {
  dispatcher_ = dispatcher;
  while (displays_.is_empty() || !has_ownership_) {
    fbl::AutoLock lock(mtx());
    class EventHandler : public fidl::WireSyncEventHandler<fhd::Coordinator> {
     public:
      explicit EventHandler(TestFidlClient* client) : client_(client) {}

      bool ok() const { return ok_; }

      void OnDisplaysChanged(fidl::WireEvent<fhd::Coordinator::OnDisplaysChanged>* event) override {
        for (size_t i = 0; i < event->added.count(); i++) {
          client_->displays_.push_back(Display(event->added[i]));
        }
      }

      void OnVsync(fidl::WireEvent<fhd::Coordinator::OnVsync>* event) override { ok_ = false; }

      void OnClientOwnershipChange(
          fidl::WireEvent<fhd::Coordinator::OnClientOwnershipChange>* event) override {
        client_->has_ownership_ = event->has_ownership;
      }

     private:
      TestFidlClient* const client_;
      bool ok_ = true;
    };

    EventHandler event_handler(this);
    auto result = dc_.HandleOneEvent(event_handler);
    if (!result.ok() || !event_handler.ok()) {
      zxlogf(ERROR, "Got unexpected message");
      return false;
    }
  }

  fbl::AutoLock lock(mtx());
  EXPECT_TRUE(has_ownership_);
  EXPECT_FALSE(displays_.is_empty());

  event_msg_wait_event_.set_object(dc_.client_end().channel().get());
  event_msg_wait_event_.set_trigger(ZX_CHANNEL_READABLE);
  EXPECT_OK(event_msg_wait_event_.Begin(dispatcher));
  return dc_->EnableVsync(true).ok();
}

void TestFidlClient::OnEventMsgAsync(async_dispatcher_t* dispatcher, async::WaitBase* self,
                                     zx_status_t status, const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    return;
  }

  if (!(signal->observed & ZX_CHANNEL_READABLE)) {
    return;
  }

  fbl::AutoLock lock(mtx());
  class EventHandler : public fidl::WireSyncEventHandler<fhd::Coordinator> {
   public:
    explicit EventHandler(TestFidlClient* client) : client_(client) {}

    void OnDisplaysChanged(fidl::WireEvent<fhd::Coordinator::OnDisplaysChanged>* event) override {}

    // The FIDL bindings do not know that the caller holds mtx(), so we can't TA_REQ(mtx()) here.
    void OnVsync(fidl::WireEvent<fhd::Coordinator::OnVsync>* event) override
        TA_NO_THREAD_SAFETY_ANALYSIS {
      client_->vsync_count_++;
      client_->recent_presented_config_stamp_ = event->applied_config_stamp;
      VsyncAckCookie vsync_ack_cookie = ToVsyncAckCookie(event->cookie);
      if (vsync_ack_cookie != kInvalidVsyncAckCookie) {
        client_->vsync_ack_cookie_ = vsync_ack_cookie;
      }
    }

    void OnClientOwnershipChange(
        fidl::WireEvent<fhd::Coordinator::OnClientOwnershipChange>* message) override {}

   private:
    TestFidlClient* const client_;
  };

  EventHandler event_handler(this);
  auto result = dc_.HandleOneEvent(event_handler);

  if (!result.ok()) {
    zxlogf(ERROR, "Failed to handle events: %s", result.FormatDescription().c_str());
    return;
  }

  if (event_msg_wait_event_.object() == ZX_HANDLE_INVALID) {
    return;
  }
  // Re-arm the wait.
  self->Begin(dispatcher);
}

TestFidlClient::~TestFidlClient() {
  if (dispatcher_) {
    // Cancel must be issued from the dispatcher thread.
    sync_completion_t done;
    auto task = new async::Task();
    task->set_handler(
        [this, task, done_ptr = &done](async_dispatcher_t*, async::Task*, zx_status_t) {
          event_msg_wait_event_.Cancel();
          event_msg_wait_event_.set_object(ZX_HANDLE_INVALID);

          sync_completion_signal(done_ptr);
          delete task;
        });
    if (task->Post(dispatcher_) != ZX_OK) {
      delete task;
      event_msg_wait_event_.Cancel();
      event_msg_wait_event_.set_object(ZX_HANDLE_INVALID);
    } else {
      while (true) {
        if (sync_completion_wait(&done, ZX_MSEC(10)) == ZX_OK) {
          break;
        }
      }
    }
  }
}

zx_status_t TestFidlClient::PresentLayers(std::vector<PresentLayerInfo> present_layers) {
  fbl::AutoLock l(mtx());

  std::vector<fhd::wire::LayerId> fidl_layers;
  for (const auto& info : present_layers) {
    fidl_layers.push_back(ToFidlLayerId(info.layer_id));
  }
  if (auto reply =
          dc_->SetDisplayLayers(ToFidlDisplayId(display_id()),
                                fidl::VectorView<fhd::wire::LayerId>::FromExternal(fidl_layers));
      !reply.ok()) {
    return reply.status();
  }

  for (const auto& info : present_layers) {
    const fhd::wire::LayerId fidl_layer_id = ToFidlLayerId(info.layer_id);
    const EventId wait_event_id = info.image_ready_wait_event_id.value_or(kInvalidEventId);
    if (auto reply = dc_->SetLayerImage(fidl_layer_id, ToFidlImageId(info.image_id),
                                        /*wait_event_id=*/ToFidlEventId(wait_event_id),
                                        /*signal_event_id=*/ToFidlEventId(kInvalidEventId));
        !reply.ok()) {
      return reply.status();
    }
  }

  if (auto reply = dc_->CheckConfig(false);
      !reply.ok() || reply.value().res != fhd::wire::ConfigResult::kOk) {
    return reply.ok() ? ZX_ERR_INVALID_ARGS : reply.status();
  }
  return dc_->ApplyConfig().status();
}

fuchsia_hardware_display::wire::ConfigStamp TestFidlClient::GetRecentAppliedConfigStamp() {
  fbl::AutoLock lock(mtx());
  EXPECT_TRUE(dc_);
  auto result = dc_->GetLatestAppliedConfigStamp();
  EXPECT_TRUE(result.ok());
  return result.value().stamp;
}

zx::result<ImageId> TestFidlClient::ImportImageWithSysmem(
    const fhd::wire::ImageConfig& image_config) {
  fbl::AutoLock lock(mtx());
  return ImportImageWithSysmemLocked(image_config);
}

std::vector<TestFidlClient::PresentLayerInfo> TestFidlClient::CreateDefaultPresentLayerInfo() {
  zx::result<LayerId> layer_result = CreateLayer();
  EXPECT_OK(layer_result.status_value());

  zx::result<ImageId> image_result = ImportImageWithSysmem(displays_[0].image_config_);
  EXPECT_OK(image_result.status_value());

  return {
      {.layer_id = layer_result.value(),
       .image_id = image_result.value(),
       .image_ready_wait_event_id = std::nullopt},
  };
}

zx::result<ImageId> TestFidlClient::ImportImageWithSysmemLocked(
    const fhd::wire::ImageConfig& image_config) {
  // Create all the tokens.
  fidl::WireSyncClient<sysmem::BufferCollectionToken> local_token;
  {
    zx::result endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "Failed to create channel for shared collection: %s",
             endpoints.status_string());
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    auto& [client, server] = endpoints.value();
    auto result = sysmem_->AllocateSharedCollection(std::move(server));
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to allocate shared collection: %s", result.status_string());
      return zx::error(result.status());
    }
    local_token = fidl::WireSyncClient<sysmem::BufferCollectionToken>(std::move(client));
    EXPECT_NE(ZX_HANDLE_INVALID, local_token.client_end().channel().get());
  }
  zx::result endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create channel for shared collection: %s", endpoints.status_string());
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  auto& [client, server] = endpoints.value();
  {
    if (auto result = local_token->Duplicate(ZX_RIGHT_SAME_RIGHTS, std::move(server));
        !result.ok()) {
      zxlogf(ERROR, "Failed to duplicate token: %s", result.FormatDescription().c_str());
      return zx::error(ZX_ERR_NO_MEMORY);
    }
  }

  // Set display buffer constraints.
  static BufferCollectionId next_display_collection_id(0);
  const BufferCollectionId display_collection_id = ++next_display_collection_id;
  if (auto result = local_token->Sync(); !result.ok()) {
    zxlogf(ERROR, "Failed to sync token %d %s", result.status(),
           result.FormatDescription().c_str());
    return zx::error(result.status());
  }

  const fuchsia_hardware_display::wire::BufferCollectionId fidl_display_collection_id =
      ToFidlBufferCollectionId(display_collection_id);
  if (auto result = dc_->ImportBufferCollection(fidl_display_collection_id, std::move(client));
      !result.ok() || result.value().res != ZX_OK) {
    zxlogf(ERROR, "Failed to import buffer collection %lu (fidl=%d, res=%d)",
           display_collection_id.value(), result.status(), result.value().res);
    return zx::error(result.ok() ? result.value().res : result.status());
  }

  auto set_constraints_result =
      dc_->SetBufferCollectionConstraints(fidl_display_collection_id, image_config);
  if (!set_constraints_result.ok() || set_constraints_result.value().res != ZX_OK) {
    zxlogf(ERROR, "Setting buffer (%dx%d) collection constraints failed: %s", image_config.width,
           image_config.height, set_constraints_result.FormatDescription().c_str());
    (void)dc_->ReleaseBufferCollection(fidl_display_collection_id);
    return zx::error(set_constraints_result.ok() ? set_constraints_result.value().res
                                                 : set_constraints_result.status());
  }

  // Use the local collection so we can read out the error if allocation
  // fails, and to ensure everything's allocated before trying to import it
  // into another process.
  fidl::WireSyncClient<sysmem::BufferCollection> sysmem_collection;
  {
    zx::result endpoints = fidl::CreateEndpoints<sysmem::BufferCollection>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "Failed to create channel for shared collection: %s",
             endpoints.status_string());
      return zx::error(ZX_ERR_NO_MEMORY);
    }
    auto& [client, server] = endpoints.value();
    if (auto result = sysmem_->BindSharedCollection(local_token.TakeClientEnd(), std::move(server));
        !result.ok()) {
      zxlogf(ERROR, "Failed to bind shared collection: %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
    sysmem_collection = fidl::WireSyncClient<sysmem::BufferCollection>(std::move(client));
  }
  // TODO(fxbug.dev/97955) Consider handling the error instead of ignoring it.
  (void)sysmem_collection->SetName(10000u, "display-client-unittest");
  sysmem::wire::BufferCollectionConstraints constraints = {};
  constraints.min_buffer_count = 1;
  constraints.usage.none = sysmem::wire::kNoneUsage;
  // We specify min_size_bytes 1 so that something is specifying a minimum size.  More typically the
  // display client would specify ImageFormatConstraints that implies a non-zero min_size_bytes.
  constraints.has_buffer_memory_constraints = true;
  constraints.buffer_memory_constraints.min_size_bytes = 1;
  constraints.buffer_memory_constraints.ram_domain_supported = true;
  zx_status_t status = sysmem_collection->SetConstraints(true, constraints).status();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Unable to set constraints (%d)", status);
    return zx::error(status);
  }
  // Wait for the buffers to be allocated.
  auto info_result = sysmem_collection->WaitForBuffersAllocated();
  if (!info_result.ok() || info_result.value().status != ZX_OK) {
    zxlogf(ERROR, "Waiting for buffers failed (fidl=%d res=%d)", info_result.status(),
           info_result.value().status);
    return zx::error(info_result.ok() ? info_result.value().status : info_result.status());
  }

  auto& info = info_result.value().buffer_collection_info;
  if (info.buffer_count < 1) {
    zxlogf(ERROR, "Incorrect buffer collection count %d", info.buffer_count);
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  const ImageId image_id = next_image_id_++;
  const fhd::wire::ImageId fidl_image_id = ToFidlImageId(image_id);
  auto import_result = dc_->ImportImage(image_config,
                                        fhd::wire::BufferId{
                                            .buffer_collection_id = fidl_display_collection_id,
                                            .buffer_index = 0,
                                        },
                                        fidl_image_id);
  if (!import_result.ok() || import_result.value().res != ZX_OK) {
    zxlogf(ERROR, "Importing image failed (fidl=%d, res=%d)", import_result.status(),
           import_result.value().res);
    return zx::error(import_result.ok() ? import_result.value().res : import_result.status());
  }

  // TODO(fxbug.dev/97955) Consider handling the error instead of ignoring it.
  (void)sysmem_collection->Close();
  return zx::ok(image_id);
}

}  // namespace display
