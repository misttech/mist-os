// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PROXY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PROXY_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/fit/function.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/sync/completion.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>
#include <list>
#include <memory>
#include <span>
#include <vector>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/ring_buffer.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

namespace display_coordinator {

class Controller;

// ClientProxy manages interactions between its Client instance and the
// controller. Methods on this class are thread safe.
class ClientProxy {
 public:
  // `client_id` is assigned by the Controller to distinguish clients.
  // `controller` must outlive ClientProxy.
  ClientProxy(Controller* controller, ClientPriority client_priority, ClientId client_id,
              fit::function<void()> on_client_disconnected);

  ~ClientProxy();

  zx_status_t Init(inspect::Node* parent_node,
                   fidl::ServerEnd<fuchsia_hardware_display::Coordinator> server_end,
                   fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
                       coordinator_listener_client_end);

  zx::result<> InitForTesting(fidl::ServerEnd<fuchsia_hardware_display::Coordinator> server_end,
                              fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
                                  coordinator_listener_client_end);

  // Tears down the `Client` instance.
  //
  // Must be called on the driver dispatcher.
  void TearDown();

  // Requires holding `controller_.mtx()` lock.
  void OnDisplayVsync(display::DisplayId display_id, zx_instant_mono_t timestamp,
                      display::DriverConfigStamp driver_config_stamp);
  void OnDisplaysChanged(std::span<const display::DisplayId> added_display_ids,
                         std::span<const display::DisplayId> removed_display_ids);
  void SetOwnership(bool is_owner);
  void OnCaptureComplete();

  // See `Client::ReapplyConfig()`.
  void ReapplyConfig();

  void EnableVsyncEventDelivery() {
    fbl::AutoLock lock(&mtx_);
    vsync_delivery_enabled_ = true;
  }

  void EnableCapture(bool enable) {
    fbl::AutoLock lock(&mtx_);
    enable_capture_ = enable;
  }
  void OnClientDead();

  // This function restores client configurations that are not part of
  // the standard configuration. These configurations are typically one-time
  // settings that need to get restored once the client takes control again.
  void ReapplySpecialConfigs();

  ClientId client_id() const { return handler_.id(); }
  ClientPriority client_priority() const { return handler_.priority(); }

  inspect::Node& node() { return node_; }

  struct ConfigStampPair {
    display::DriverConfigStamp driver_stamp;
    display::ConfigStamp client_stamp;
  };
  std::list<ConfigStampPair>& pending_applied_config_stamps() {
    return pending_applied_config_stamps_;
  }

  // Add a new mapping entry from `stamps.controller_stamp` to `stamp.config_stamp`.
  // Controller should guarantee that `stamps.controller_stamp` is strictly
  // greater than existing pending controller stamps.
  void UpdateConfigStampMapping(ConfigStampPair stamps);

  void CloseForTesting();

  display::VsyncAckCookie LastVsyncAckCookieForTesting();

  // Fired after the FIDL client is unbound.
  sync_completion_t* FidlUnboundCompletionForTesting();

  size_t ImportedImagesCountForTesting() const { return handler_.ImportedImagesCountForTesting(); }

  // Define these constants here so we can access them in tests.

  static constexpr uint32_t kVsyncBufferSize = 10;

  // Maximum number of vsync messages sent before an acknowledgement is required.
  // Half of this limit is provided to clients as part of display info. Assuming a
  // frame rate of 60hz, clients will be required to acknowledge at least once a second
  // and driver will stop sending messages after 2 seconds of no acknowledgement
  static constexpr uint32_t kMaxVsyncMessages = 120;
  static constexpr uint32_t kVsyncMessagesWatermark = (kMaxVsyncMessages / 2);
  // At the moment, maximum image handles returned by any driver is 4 which is
  // equal to number of hardware layers. 8 should be more than enough to allow for
  // a simple statically allocated array of image_ids for vsync events that are being
  // stored due to client non-acknowledgement.
  static constexpr uint32_t kMaxImageHandles = 8;

 private:
  fbl::Mutex mtx_;
  Controller& controller_;

  Client handler_;
  bool vsync_delivery_enabled_ __TA_GUARDED(&mtx_) = false;
  bool enable_capture_ __TA_GUARDED(&mtx_) = false;

  fbl::Mutex task_mtx_;
  std::vector<std::unique_ptr<async::Task>> client_scheduled_tasks_ __TA_GUARDED(task_mtx_);

  struct VsyncMessageData {
    display::DisplayId display_id;
    zx_instant_mono_t timestamp;
    display::ConfigStamp config_stamp;
  };

  fbl::RingBuffer<VsyncMessageData, kVsyncBufferSize> buffered_vsync_messages_;
  uint64_t vsync_cookie_salt_ = 0;
  uint64_t vsync_cookie_sequence_ = 0;

  uint64_t number_of_vsyncs_sent_ = 0;
  display::VsyncAckCookie last_cookie_sent_ = display::kInvalidVsyncAckCookie;
  bool acknowledge_request_sent_ = false;

  fit::function<void()> on_client_disconnected_;

  // Fired when the FIDL connection is unbound.
  //
  // This member is thread-safe.
  sync_completion_t fidl_unbound_completion_;

  // Mapping from controller_stamp to client_stamp for all configurations that
  // are already applied and pending to be presented on the display.
  // Ordered by `controller_stamp_` in increasing order.
  std::list<ConfigStampPair> pending_applied_config_stamps_;

  inspect::Node node_;
  inspect::BoolProperty is_owner_property_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PROXY_H_
