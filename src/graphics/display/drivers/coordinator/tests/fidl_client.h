// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_FIDL_CLIENT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_FIDL_CLIENT_H_

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

#include "src/graphics/display/drivers/coordinator/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace display {

class TestFidlClient {
 public:
  class Display {
   public:
    explicit Display(const fuchsia_hardware_display::wire::Info& info);

    DisplayId id_;
    fbl::Vector<fuchsia_images2::wire::PixelFormat> pixel_formats_;
    fbl::Vector<fuchsia_hardware_display::wire::Mode> modes_;
    fbl::Vector<fuchsia_hardware_display::wire::CursorInfo> cursors_;

    fbl::String manufacturer_name_;
    fbl::String monitor_name_;
    fbl::String monitor_serial_;

    fuchsia_hardware_display::wire::ImageConfig image_config_;
  };

  explicit TestFidlClient(const fidl::WireSyncClient<fuchsia_sysmem::Allocator>& sysmem)
      : sysmem_(sysmem) {}

  ~TestFidlClient();

  bool CreateChannel(const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& provider,
                     bool is_vc);
  // Enable vsync for a display and wait for events using |dispatcher|.
  bool Bind(async_dispatcher_t* dispatcher) TA_EXCL(mtx());

  struct EventInfo {
    uint64_t id;
    zx::event event = {};
  };

  zx::result<uint64_t> ImportImageWithSysmem(
      const fuchsia_hardware_display::wire::ImageConfig& image_config) TA_EXCL(mtx());

  zx::result<uint64_t> CreateImage() TA_EXCL(mtx());
  zx::result<uint64_t> CreateLayer() TA_EXCL(mtx());
  zx::result<EventInfo> CreateEvent() TA_EXCL(mtx());

  fuchsia_hardware_display::wire::ConfigStamp GetRecentAppliedConfigStamp() TA_EXCL(mtx());

  struct PresentLayerInfo {
    uint64_t layer_id;
    uint64_t image_id;
    std::optional<uint64_t> image_ready_wait_event_id;
  };

  std::vector<PresentLayerInfo> CreateDefaultPresentLayerInfo();

  zx_status_t PresentLayers() { return PresentLayers(CreateDefaultPresentLayerInfo()); }

  zx_status_t PresentLayers(std::vector<PresentLayerInfo> layers);

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
    return ToConfigStamp(recent_presented_config_stamp_);
  }

  uint64_t get_cookie() const { return cookie_; }

  fbl::Mutex* mtx() const { return &mtx_; }

 private:
  mutable fbl::Mutex mtx_;
  async_dispatcher_t* dispatcher_ = nullptr;
  uint64_t vsync_count_ TA_GUARDED(mtx()) = 0;
  uint64_t cookie_ = 0;
  uint64_t next_image_id_ TA_GUARDED(mtx()) = 1;
  fuchsia_hardware_display::wire::ConfigStamp recent_presented_config_stamp_;
  const fidl::WireSyncClient<fuchsia_sysmem::Allocator>& sysmem_;

  zx::result<uint64_t> ImportImageWithSysmemLocked(
      const fuchsia_hardware_display::wire::ImageConfig& image_config) TA_REQ(mtx());
  zx::result<uint64_t> CreateLayerLocked() TA_REQ(mtx());
  zx::result<EventInfo> CreateEventLocked() TA_REQ(mtx());

  void OnEventMsgAsync(async_dispatcher_t* dispatcher, async::WaitBase* self, zx_status_t status,
                       const zx_packet_signal_t* signal) TA_EXCL(mtx());
  async::WaitMethod<TestFidlClient, &TestFidlClient::OnEventMsgAsync> event_msg_wait_event_{this};
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_FIDL_CLIENT_H_
