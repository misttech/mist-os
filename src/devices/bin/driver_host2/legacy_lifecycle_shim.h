// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST2_LEGACY_LIFECYCLE_SHIM_H_
#define SRC_DEVICES_BIN_DRIVER_HOST2_LEGACY_LIFECYCLE_SHIM_H_

#include <fidl/fuchsia.driver.framework/cpp/driver/wire.h>
#include <lib/driver/symbols/symbols.h>

#include <fbl/auto_lock.h>

namespace dfv2 {

// This class provides a shim for drivers with the legacy lifecycle mechanism (C-ABI based).
// It provides the new Driver FIDL and converts the calls it receives into the C functions that
// the legacy lifecycle struct expects.
//
// # Thread Safety
//
// The class itself will be running on the driver host's main async loop dispatcher, while
// it serves the Driver FIDL through a separate |dispatcher_| that belongs to the driver.
// This is only safe to do, because we ensure to keep this class alive until after
// the driver's |dispatcher_| has fully shutdown. We will also ensure to use locks on fields
// that will be accessed from both contexts.
class LegacyLifecycleShim : public fdf::WireServer<fuchsia_driver_framework::Driver> {
 public:
  explicit LegacyLifecycleShim(const DriverLifecycle* lifecycle, std::string_view name);

  void Initialize(fdf_dispatcher_t* dispatcher,
                  fdf::ServerEnd<fuchsia_driver_framework::Driver> server_end);
  zx_status_t Destroy();

  // fdf::WireServer<fuchsia_driver_framework::Driver>
  // Runs on the |dispatcher_|.
  void Start(StartRequestView request, fdf::Arena& arena, StartCompleter::Sync& completer) override;

  // fdf::WireServer<fuchsia_driver_framework::Driver>
  // Runs on the |dispatcher_|.
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override;

  // fdf::WireServer<fuchsia_driver_framework::Driver>
  // Runs on the |dispatcher_|.
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_framework::Driver> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  const DriverLifecycle* lifecycle_;
  std::string name_;
  fdf_dispatcher_t* dispatcher_;

  // This is only accessed from |dispatcher_|.
  // This is the server binging for the Driver FIDL. This class converts the requests
  // for this into the C function calls the driver expects.
  std::optional<fdf::ServerBinding<fuchsia_driver_framework::Driver>> binding_;

  // This is only accessed from |dispatcher_|.
  // This will be used to store the async Start completer while it is in progress.
  std::optional<StartCompleter::Async> start_completer_;
  std::optional<fdf::Arena> start_arena_;

  // Used for shared fields.
  fbl::Mutex lock_;

  // This will be set from |dispatcher_|, but will be read from outside
  std::optional<void*> opaque_ __TA_GUARDED(lock_);
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_HOST2_LEGACY_LIFECYCLE_SHIM_H_
