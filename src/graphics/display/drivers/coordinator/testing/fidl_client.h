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
#include <zircon/types.h>

#include <initializer_list>
#include <memory>

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

namespace display {

class TestFidlClient {
 public:
  class Display {
   public:
    explicit Display(const fuchsia_hardware_display::wire::Info& info);

    DisplayId id_;
    fbl::Vector<fuchsia_images2::wire::PixelFormat> pixel_formats_;
    fbl::Vector<fuchsia_hardware_display::wire::Mode> modes_;

    fbl::String manufacturer_name_;
    fbl::String monitor_name_;
    fbl::String monitor_serial_;

    fuchsia_hardware_display_types::wire::ImageMetadata image_metadata_;
  };

  struct EventInfo {
    EventId id;
    zx::event event;
  };

  explicit TestFidlClient(const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem)
      : sysmem_(sysmem) {}

  ~TestFidlClient();

  zx::result<> OpenCoordinator(
      const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
      ClientPriority client_priority, async_dispatcher_t& coordinator_listener_dispatcher);

  bool HasOwnershipAndValidDisplay() const;

  zx::result<> EnableVsync();

  zx::result<ImageId> ImportImageWithSysmem(
      const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) TA_EXCL(mtx());

  zx::result<ImageId> CreateImage() TA_EXCL(mtx());
  zx::result<LayerId> CreateLayer() TA_EXCL(mtx());
  zx::result<EventInfo> CreateEvent() TA_EXCL(mtx());

  fuchsia_hardware_display_types::wire::ConfigStamp GetRecentAppliedConfigStamp() TA_EXCL(mtx());

  struct PresentLayerInfo {
    LayerId layer_id;
    ImageId image_id;
    std::optional<EventId> image_ready_wait_event_id;
  };

  std::vector<PresentLayerInfo> CreateDefaultPresentLayerInfo();

  zx_status_t PresentLayers() { return PresentLayers(CreateDefaultPresentLayerInfo()); }

  zx_status_t PresentLayers(std::vector<PresentLayerInfo> layers);

  void OnDisplaysChanged(std::vector<fuchsia_hardware_display::wire::Info> added_displays,
                         std::vector<DisplayId> removed_display_ids);

  void OnClientOwnershipChange(bool has_ownership);

  void OnVsync(DisplayId display_id, zx::time timestamp, ConfigStamp applied_config_stamp,
               VsyncAckCookie vsync_ack_cookie);

  DisplayId display_id() const;

  fbl::Vector<Display> displays_;
  fidl::WireSyncClient<fuchsia_hardware_display::Coordinator> dc_ TA_GUARDED(mtx());
  bool has_ownership_ = false;

  uint64_t vsync_count() const {
    fbl::AutoLock lock(mtx());
    return vsync_count_;
  }

  ConfigStamp recent_presented_config_stamp() const {
    fbl::AutoLock lock(mtx());
    return recent_presented_config_stamp_;
  }

  VsyncAckCookie vsync_ack_cookie() const { return vsync_ack_cookie_; }

  fbl::Mutex* mtx() const { return &mtx_; }

 private:
  mutable fbl::Mutex mtx_;
  uint64_t vsync_count_ TA_GUARDED(mtx()) = 0;
  VsyncAckCookie vsync_ack_cookie_ = kInvalidVsyncAckCookie;
  ImageId next_image_id_ TA_GUARDED(mtx()) = ImageId(1);
  ConfigStamp recent_presented_config_stamp_;
  const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem_;

  MockCoordinatorListener coordinator_listener_{
      fit::bind_member<&TestFidlClient::OnDisplaysChanged>(this),
      fit::bind_member<&TestFidlClient::OnVsync>(this),
      fit::bind_member<&TestFidlClient::OnClientOwnershipChange>(this)};

  zx::result<ImageId> ImportImageWithSysmemLocked(
      const fuchsia_hardware_display_types::wire::ImageMetadata& image_metadata) TA_REQ(mtx());
  zx::result<LayerId> CreateLayerLocked() TA_REQ(mtx());
  zx::result<EventInfo> CreateEventLocked() TA_REQ(mtx());
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_FIDL_CLIENT_H_
