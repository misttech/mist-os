// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_FIDL_CLIENT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_FIDL_CLIENT_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/fidl/cpp/message.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/event.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/string.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/testing/mock-coordinator-listener.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/layer-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace display_coordinator {

class TestFidlClient {
 public:
  class Display {
   public:
    explicit Display(const fuchsia_hardware_display::wire::Info& info);

    display::DisplayId id_;
    fbl::Vector<fuchsia_images2::wire::PixelFormat> pixel_formats_;
    fbl::Vector<fuchsia_hardware_display_types::wire::Mode> modes_;

    fbl::String manufacturer_name_;
    fbl::String monitor_name_;
    fbl::String monitor_serial_;

    fuchsia_hardware_display_types::wire::ImageMetadata image_metadata_;
  };

  struct EventInfo {
    display::EventId id;
    zx::event event;
  };

  explicit TestFidlClient(const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem)
      : sysmem_(sysmem) {}

  ~TestFidlClient();

  // `coordinator_listener_dispatcher` must be non-null and must be running
  // throughout the test.
  zx::result<> OpenCoordinator(
      const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
      ClientPriority client_priority, async_dispatcher_t* coordinator_listener_dispatcher);

  bool HasOwnershipAndValidDisplay() const;

  zx::result<> EnableVsyncEventDelivery();

  zx::result<display::ImageId> ImportImageWithSysmem(
      const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) TA_EXCL(mtx());

  zx::result<display::ImageId> CreateImage() TA_EXCL(mtx());
  zx::result<display::LayerId> CreateLayer() TA_EXCL(mtx());
  zx::result<EventInfo> CreateEvent() TA_EXCL(mtx());

  fuchsia_hardware_display_types::wire::ConfigStamp GetRecentAppliedConfigStamp() TA_EXCL(mtx());

  struct PresentLayerInfo {
    display::LayerId layer_id;
    display::ImageId image_id;
    std::optional<display::EventId> image_ready_wait_event_id;
  };

  std::vector<PresentLayerInfo> CreateDefaultPresentLayerInfo();

  zx_status_t PresentLayers() { return PresentLayers(CreateDefaultPresentLayerInfo()); }

  zx_status_t PresentLayers(std::vector<PresentLayerInfo> layers);

  void OnDisplaysChanged(std::vector<fuchsia_hardware_display::wire::Info> added_displays,
                         std::vector<display::DisplayId> removed_display_ids);

  void OnClientOwnershipChange(bool has_ownership);

  void OnVsync(display::DisplayId display_id, zx::time timestamp,
               display::ConfigStamp applied_config_stamp, display::VsyncAckCookie vsync_ack_cookie);

  display::DisplayId display_id() const;

  fbl::Vector<Display> displays_;
  fidl::WireSyncClient<fuchsia_hardware_display::Coordinator> dc_ TA_GUARDED(mtx());
  bool has_ownership_ = false;

  uint64_t vsync_count() const {
    fbl::AutoLock lock(mtx());
    return vsync_count_;
  }

  display::ConfigStamp recent_presented_config_stamp() const {
    fbl::AutoLock lock(mtx());
    return recent_presented_config_stamp_;
  }

  display::VsyncAckCookie vsync_ack_cookie() const { return vsync_ack_cookie_; }

  fbl::Mutex* mtx() const { return &mtx_; }

 private:
  mutable fbl::Mutex mtx_;
  uint64_t vsync_count_ TA_GUARDED(mtx()) = 0;
  display::VsyncAckCookie vsync_ack_cookie_ = display::kInvalidVsyncAckCookie;
  display::ImageId next_image_id_ TA_GUARDED(mtx()) = display::ImageId(1);
  display::ConfigStamp recent_presented_config_stamp_;
  const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem_;

  MockCoordinatorListener coordinator_listener_{
      fit::bind_member<&TestFidlClient::OnDisplaysChanged>(this),
      fit::bind_member<&TestFidlClient::OnVsync>(this),
      fit::bind_member<&TestFidlClient::OnClientOwnershipChange>(this)};

  zx::result<display::ImageId> ImportImageWithSysmemLocked(
      const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) TA_REQ(mtx());
  zx::result<display::LayerId> CreateLayerLocked() TA_REQ(mtx());
  zx::result<EventInfo> CreateEventLocked() TA_REQ(mtx());
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_FIDL_CLIENT_H_
