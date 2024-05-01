// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/audiotypes/c/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/channel.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstdlib>
#include <list>
#include <memory>

#include <fbl/array.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/capture-image.h"
#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/display-info.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/drivers/coordinator/vsync-monitor.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/lib/async-watchdog/watchdog.h"

namespace display {

class ClientProxy;
class Controller;
class ControllerTest;
class DisplayConfig;
class IntegrationTest;

// Multiplexes between display controller clients and display engine drivers.
class Controller : public ddk::DisplayControllerInterfaceProtocol<Controller>,
                   public fidl::WireServer<fuchsia_hardware_display::Provider> {
 public:
  // Factory method for production use.
  // Creates and initializes a Controller instance.
  static zx::result<std::unique_ptr<Controller>> Create(
      std::unique_ptr<EngineDriverClient> engine_driver_client);

  // Creates a new coordinator Controller instance. It creates a new Inspector
  // which will be solely owned by the Controller instance.
  //
  // `engine_driver_client` must not be null.
  explicit Controller(std::unique_ptr<EngineDriverClient> engine_driver_client);

  // Creates a new coordinator Controller instance with an injected `inspector`.
  // The `inspector` and inspect data may be duplicated and shared.
  //
  // `engine_driver_client` must not be null.
  Controller(std::unique_ptr<EngineDriverClient> engine_driver_client,
             inspect::Inspector inspector);

  Controller(const Controller&) = delete;
  Controller& operator=(const Controller&) = delete;

  ~Controller() override;

  static void PopulateDisplayMode(const display::DisplayTiming& timing, display_mode_t* mode);

  // These method names reference the DFv2 (fdf::DriverBase) driver lifecycle.
  void PrepareStop();
  void Stop();

  void DisplayControllerInterfaceOnDisplaysChanged(
      const added_display_args_t* added_banjo_display_list, size_t added_banjo_display_count,
      const uint64_t* removed_banjo_display_id_list, size_t removed_banjo_display_id_count);

  void DisplayControllerInterfaceOnDisplayVsync(uint64_t banjo_display_id, zx_time_t timestamp,
                                                const config_stamp_t* config_stamp);

  void DisplayControllerInterfaceOnCaptureComplete();
  void OnClientDead(ClientProxy* client);
  void SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode virtcon_mode);
  void ShowActiveDisplay();

  void ApplyConfig(DisplayConfig* configs[], int32_t count, ConfigStamp config_stamp,
                   uint32_t layer_stamp, ClientId client_id) __TA_EXCLUDES(mtx());

  void ReleaseImage(DriverImageId driver_image_id);
  void ReleaseCaptureImage(DriverCaptureImageId driver_capture_image_id);

  // |mtx()| must be held for as long as |edid| and |params| are retained.
  bool GetPanelConfig(DisplayId display_id, const fbl::Vector<display::DisplayTiming>** timings,
                      const display_mode_t** mode) __TA_REQUIRES(mtx());

  zx::result<fbl::Array<CoordinatorPixelFormat>> GetSupportedPixelFormats(DisplayId display_id)
      __TA_REQUIRES(mtx());

  // Calls `callback` with a const DisplayInfo& matching the given `display_id`.
  //
  // Returns true iff a DisplayInfo with `display_id` was found and `callback`
  // was called.
  //
  // The controller mutex is guaranteed to be held while `callback` is called.
  template <typename Callback>
  bool FindDisplayInfo(DisplayId display_id, Callback callback) __TA_REQUIRES(mtx());

  EngineDriverClient* engine_driver_client() { return engine_driver_client_.get(); }

  bool supports_capture() { return supports_capture_; }

  async_dispatcher_t* async_dispatcher() { return dispatcher_.async_dispatcher(); }
  bool IsRunningOnDispatcher() { return fdf::Dispatcher::GetCurrent()->get() == dispatcher_.get(); }
  // Thread-safety annotations currently don't deal with pointer aliases. Use this to document
  // places where we believe a mutex aliases mtx()
  void AssertMtxAliasHeld(mtx_t* m) __TA_ASSERT(m) { ZX_DEBUG_ASSERT(m == mtx()); }
  mtx_t* mtx() const { return &mtx_; }
  const inspect::Inspector& inspector() const { return inspector_; }

  // Test helpers
  size_t TEST_imported_images_count() const;
  ConfigStamp TEST_controller_stamp() const;
  void SetDispatcherForTesting(fdf::SynchronizedDispatcher dispatcher) {
    dispatcher_ = std::move(dispatcher);
  }
  void ShutdownDispatcherForTesting() { dispatcher_.ShutdownAsync(); }

  // Typically called by OpenController/OpenVirtconController.  However, this is made public
  // for use by testing services which provide a fake display controller.
  zx_status_t CreateClient(ClientPriority client_priority,
                           fidl::ServerEnd<fuchsia_hardware_display::Coordinator> client,
                           fit::function<void()> on_client_dead = nullptr);

  display::DriverBufferCollectionId GetNextDriverBufferCollectionId();

  void OpenCoordinatorForVirtcon(OpenCoordinatorForVirtconRequestView request,
                                 OpenCoordinatorForVirtconCompleter::Sync& completer) override;
  void OpenCoordinatorForPrimary(OpenCoordinatorForPrimaryRequestView request,
                                 OpenCoordinatorForPrimaryCompleter::Sync& completer) override;

 private:
  friend ControllerTest;
  friend IntegrationTest;

  // Initializes logic that is not suitable for the constructor.
  zx::result<> Initialize();

  void HandleClientOwnershipChanges() __TA_REQUIRES(mtx());
  void PopulateDisplayTimings(const fbl::RefPtr<DisplayInfo>& info) __TA_EXCLUDES(mtx());

  inspect::Inspector inspector_;
  // Currently located at bootstrap/driver_manager:root/display.
  inspect::Node root_;

  VsyncMonitor vsync_monitor_;

  // mtx_ is a global lock on state shared among clients.
  mutable mtx_t mtx_;
  bool unbinding_ __TA_GUARDED(mtx()) = false;

  DisplayInfo::Map displays_ __TA_GUARDED(mtx());
  uint32_t applied_layer_stamp_ = UINT32_MAX;
  ClientId applied_client_id_ = kInvalidClientId;
  DriverCaptureImageId pending_release_capture_image_id_ = kInvalidDriverCaptureImageId;

  bool supports_capture_ = false;

  display::DriverBufferCollectionId next_driver_buffer_collection_id_ __TA_GUARDED(mtx()) =
      display::DriverBufferCollectionId(1);

  std::list<std::unique_ptr<ClientProxy>> clients_ __TA_GUARDED(mtx());
  ClientId next_client_id_ __TA_GUARDED(mtx()) = ClientId(1);

  // Pointers to instances owned by `clients_`.
  ClientProxy* active_client_ __TA_GUARDED(mtx()) = nullptr;
  ClientProxy* virtcon_client_ __TA_GUARDED(mtx()) = nullptr;
  ClientProxy* primary_client_ __TA_GUARDED(mtx()) = nullptr;

  // True iff the corresponding client can dispatch FIDL events.
  bool virtcon_client_ready_ __TA_GUARDED(mtx()) = false;
  bool primary_client_ready_ __TA_GUARDED(mtx()) = false;

  fuchsia_hardware_display::wire::VirtconMode virtcon_mode_ __TA_GUARDED(mtx()) =
      fuchsia_hardware_display::wire::VirtconMode::kInactive;

  fdf::SynchronizedDispatcher dispatcher_;
  libsync::Completion dispatcher_shutdown_completion_;

  std::unique_ptr<async_watchdog::Watchdog> watchdog_;
  std::unique_ptr<EngineDriverClient> engine_driver_client_;

  zx_time_t last_valid_apply_config_timestamp_{};
  inspect::UintProperty last_valid_apply_config_timestamp_ns_property_;
  inspect::UintProperty last_valid_apply_config_interval_ns_property_;
  inspect::UintProperty last_valid_apply_config_config_stamp_property_;

  ConfigStamp controller_stamp_ __TA_GUARDED(mtx()) = kInvalidConfigStamp;
};

template <typename Callback>
bool Controller::FindDisplayInfo(DisplayId display_id, Callback callback) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);

  for (const DisplayInfo& display : displays_) {
    if (display.id == display_id) {
      callback(display);
      return true;
    }
  }
  return false;
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_
