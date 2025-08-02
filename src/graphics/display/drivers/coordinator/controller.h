// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
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

#include <cstddef>
#include <cstdint>
#include <list>
#include <memory>
#include <span>

#include <fbl/mutex.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/added-display-info.h"
#include "src/graphics/display/drivers/coordinator/capture-image.h"
#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/display-info.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/drivers/coordinator/engine-listener-banjo-adapter.h"
#include "src/graphics/display/drivers/coordinator/engine-listener-fidl-adapter.h"
#include "src/graphics/display/drivers/coordinator/engine-listener.h"
#include "src/graphics/display/drivers/coordinator/id-map.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/vsync-monitor.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/engine-info.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display_coordinator {

class ClientProxy;
class DisplayConfig;

// Multiplexes between display controller clients and display engine drivers.
class Controller : public fidl::WireServer<fuchsia_hardware_display::Provider>,
                   public EngineListener {
 public:
  // Factory method for production use.
  // Creates and initializes a Controller instance.
  //
  // Asynchronous work that manages the state of the display clients and
  // coordinates the display state between clients and engine drivers runs on
  // `driver_dispatcher`.
  //
  // `engine_driver_client` must not be null.
  //
  // `driver_dispatcher` must be running until `PrepareStop()` is called.
  // `driver_dispatcher` must be shut down when `Stop()` is called.
  static zx::result<std::unique_ptr<Controller>> Create(
      std::unique_ptr<EngineDriverClient> engine_driver_client,
      fdf::UnownedSynchronizedDispatcher driver_dispatcher,
      fdf::UnownedSynchronizedDispatcher engine_listener_dispatcher);

  // Creates a new coordinator Controller instance. It creates a new Inspector
  // which will be solely owned by the Controller instance.
  //
  // `engine_driver_client` must not be null.
  explicit Controller(std::unique_ptr<EngineDriverClient> engine_driver_client,
                      fdf::UnownedSynchronizedDispatcher driver_dispatcher,
                      fdf::UnownedSynchronizedDispatcher engine_listener_dispatcher);

  Controller(const Controller&) = delete;
  Controller& operator=(const Controller&) = delete;

  ~Controller() override;

  // References the `PrepareStop()` method in the DFv2 (fdf::DriverBase) driver
  // lifecycle.
  void PrepareStop();

  // References the `Stop()` method in the DFv2 (fdf::DriverBase) driver
  // lifecycle.
  //
  // Must be called after `driver_dispatcher_` is shut down.
  void Stop();

  // `EngineListener`:
  // Must run on `engine_listener_dispatcher_`.
  void OnDisplayAdded(std::unique_ptr<AddedDisplayInfo> added_display_info) override;
  void OnDisplayRemoved(display::DisplayId removed_display_id) override;
  void OnCaptureComplete() override;
  void OnDisplayVsync(display::DisplayId display_id, zx::time_monotonic timestamp,
                      display::DriverConfigStamp driver_config_stamp) override;

  void OnClientDead(ClientProxy* client);
  void SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode virtcon_mode);

  void ApplyConfig(std::span<DisplayConfig*> display_configs,
                   display::ConfigStamp client_config_stamp, ClientId client_id)
      __TA_EXCLUDES(mtx());

  void ReleaseImage(display::DriverImageId driver_image_id);
  void ReleaseCaptureImage(display::DriverCaptureImageId driver_capture_image_id);

  // The display modes are guaranteed to be valid as long as the display with
  // `display_id` is valid.
  //
  // For a valid display, it's guaranteed that at least one of
  // `GetDisplayPreferredModes()` and `GetDisplayTimings()` is non-empty.
  //
  // `mtx()` must be held for as long as the return value is retained.
  zx::result<std::span<const display::ModeAndId>> GetDisplayPreferredModes(
      display::DisplayId display_id) __TA_REQUIRES(mtx());

  // The display timings are guaranteed to be valid as long as the display with
  // `display_id` is valid.
  //
  // `mtx()` must be held for as long as the return value is retained.
  zx::result<std::span<const display::DisplayTiming>> GetDisplayTimings(
      display::DisplayId display_id) __TA_REQUIRES(mtx());

  zx::result<fbl::Vector<display::PixelFormat>> GetSupportedPixelFormats(
      display::DisplayId display_id) __TA_REQUIRES(mtx());

  // Calls `callback` with a const DisplayInfo& matching the given `display_id`.
  //
  // Returns true iff a DisplayInfo with `display_id` was found and `callback`
  // was called.
  //
  // The controller mutex is guaranteed to be held while `callback` is called.
  template <typename Callback>
  bool FindDisplayInfo(display::DisplayId display_id, Callback callback) __TA_REQUIRES(mtx());

  EngineDriverClient* engine_driver_client() { return engine_driver_client_.get(); }

  // May only be called after the display engine driver is connected.
  bool supports_capture() { return engine_info_->is_capture_supported(); }

  // May only be called after the display engine driver is connected.
  const display::EngineInfo& engine_info() const { return *engine_info_; }

  fdf::UnownedSynchronizedDispatcher driver_dispatcher() const {
    return driver_dispatcher_->borrow();
  }
  bool IsRunningOnDriverDispatcher() {
    return fdf::Dispatcher::GetCurrent()->get() == driver_dispatcher_->get();
  }

  // Thread-safety annotations currently don't deal with pointer aliases. Use this to document
  // places where we believe a mutex aliases mtx()
  void AssertMtxAliasHeld(fbl::Mutex& m) __TA_ASSERT(m) { ZX_DEBUG_ASSERT(&m == mtx()); }
  fbl::Mutex* mtx() const { return &mtx_; }
  const inspect::Inspector& inspector() const { return inspector_; }

  // Typically called by OpenController/OpenVirtconController. However, this is made public
  // for use by testing services which provide a fake display controller.
  zx_status_t CreateClient(
      ClientPriority client_priority,
      fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
      fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
          coordinator_listener_client_end,
      fit::function<void()> on_client_disconnected);

  display::DriverBufferCollectionId GetNextDriverBufferCollectionId();

  // `fidl::WireServer<fuchsia_hardware_display::Provider>`:
  void OpenCoordinatorWithListenerForVirtcon(
      OpenCoordinatorWithListenerForVirtconRequestView request,
      OpenCoordinatorWithListenerForVirtconCompleter::Sync& completer) override;
  void OpenCoordinatorWithListenerForPrimary(
      OpenCoordinatorWithListenerForPrimaryRequestView request,
      OpenCoordinatorWithListenerForPrimaryCompleter::Sync& completer) override;

 private:
  // Initializes logic that is not suitable for the constructor.
  // Must not run on `engine_listener_dispatcher_`.
  zx::result<> Initialize();

  void HandleClientOwnershipChanges() __TA_REQUIRES(mtx());

  // Processes a display addition notification from an engine driver.
  //
  // Must be called on the driver dispatcher.
  void AddDisplay(std::unique_ptr<AddedDisplayInfo> added_display_info);

  // Processes a display removal notification from an engine driver.
  //
  // Must be called on the driver dispatcher.
  void RemoveDisplay(display::DisplayId removed_display_id);

  // Must be called on the driver dispatcher.
  void PopulateDisplayTimings(DisplayInfo& info) __TA_EXCLUDES(mtx());

  // Processes a VSync signal from an engine driver.
  //
  // Must be called on the driver dispatcher.
  void ProcessDisplayVsync(display::DisplayId display_id, zx::time_monotonic timestamp,
                           display::DriverConfigStamp driver_config_stamp);

  inspect::Inspector inspector_;
  // Currently located at bootstrap/driver_manager:root/display.
  inspect::Node root_;

  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  fdf::UnownedSynchronizedDispatcher engine_listener_dispatcher_;

  EngineListenerBanjoAdapter engine_listener_banjo_adapter_;
  EngineListenerFidlAdapter engine_listener_fidl_adapter_;

  VsyncMonitor vsync_monitor_;

  // mtx_ is a global lock on state shared among clients.
  mutable fbl::Mutex mtx_;
  bool unbinding_ __TA_GUARDED(mtx()) = false;

  DisplayInfo::Map displays_ __TA_GUARDED(mtx());
  ClientId applied_client_id_ = kInvalidClientId;
  display::DriverCaptureImageId pending_release_capture_image_id_ =
      display::kInvalidDriverCaptureImageId;

  // Populated after the engine is initialized.
  std::optional<display::EngineInfo> engine_info_;

  display::DriverBufferCollectionId next_driver_buffer_collection_id_ __TA_GUARDED(mtx()) =
      display::DriverBufferCollectionId(1);

  std::list<std::unique_ptr<ClientProxy>> clients_ __TA_GUARDED(mtx());
  ClientId next_client_id_ __TA_GUARDED(mtx()) = ClientId(1);

  // Pointers to instances owned by `clients_`.
  ClientProxy* client_owning_displays_ __TA_GUARDED(mtx()) = nullptr;
  ClientProxy* virtcon_client_ __TA_GUARDED(mtx()) = nullptr;
  ClientProxy* primary_client_ __TA_GUARDED(mtx()) = nullptr;

  // True iff the corresponding client can dispatch FIDL events.
  bool virtcon_client_ready_ __TA_GUARDED(mtx()) = false;
  bool primary_client_ready_ __TA_GUARDED(mtx()) = false;

  fuchsia_hardware_display::wire::VirtconMode virtcon_mode_ __TA_GUARDED(mtx()) =
      fuchsia_hardware_display::wire::VirtconMode::kFallback;

  std::unique_ptr<EngineDriverClient> engine_driver_client_;

  zx_instant_mono_t last_valid_apply_config_timestamp_{};
  inspect::UintProperty last_valid_apply_config_timestamp_ns_property_;
  inspect::UintProperty last_valid_apply_config_interval_ns_property_;
  inspect::UintProperty last_valid_apply_config_config_stamp_property_;

  display::DriverConfigStamp last_issued_driver_config_stamp_ __TA_GUARDED(mtx()) =
      display::kInvalidDriverConfigStamp;
};

template <typename Callback>
bool Controller::FindDisplayInfo(display::DisplayId display_id, Callback callback) {
  for (const DisplayInfo& display : displays_) {
    if (display.id() == display_id) {
      callback(display);
      return true;
    }
  }
  return false;
}

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CONTROLLER_H_
