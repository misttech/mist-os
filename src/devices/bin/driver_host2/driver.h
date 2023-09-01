// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_H_
#define SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_H_

#include <fidl/fuchsia.driver.host/cpp/fidl.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

namespace dfv2 {

class Driver : public fidl::Server<fuchsia_driver_host::Driver>,
               public fbl::RefCounted<Driver>,
               public fbl::DoublyLinkedListable<fbl::RefPtr<Driver>> {
 public:
  static zx::result<fbl::RefPtr<Driver>> Load(std::string url, zx::vmo vmo,
                                              std::string_view relative_binary_path);

  Driver(std::string url, void* library, const DriverLifecycle* lifecycle);
  ~Driver() override;

  const std::string& url() const { return url_; }
  void set_binding(fidl::ServerBindingRef<fuchsia_driver_host::Driver> binding);

  void Stop(StopCompleter::Sync& completer) override;

  // Called by the driver to signal completion of the |prepare_stop| operation.
  // |status| is the status of the prepare_stop operation sent from the driver.
  void PrepareStopCompleted(zx_status_t status);

  // Called by the driver to signal completion of the |start| operation.
  // |status| is the status of the start operation sent from the driver.
  void StartCompleted(zx_status_t status, void* opaque);

  // Starts the driver.
  void Start(fuchsia_driver_framework::DriverStartArgs start_args, fdf::Dispatcher dispatcher,
             fit::callback<void(zx::result<>)> cb);

  void ShutdownDispatcher() __TA_EXCLUDES(lock_) {
    fbl::AutoLock al(&lock_);
    initial_dispatcher_.ShutdownAsync();
  }

 private:
  std::string url_;
  void* library_;
  const DriverLifecycle* lifecycle_;

  fbl::Mutex lock_;
  std::optional<void*> opaque_ __TA_GUARDED(lock_);
  fit::callback<void(zx::result<>)> start_callback_ __TA_GUARDED(lock_);

  std::optional<fidl::ServerBindingRef<fuchsia_driver_host::Driver>> binding_ __TA_GUARDED(lock_);

  // The initial dispatcher passed to the driver.
  // This must be shutdown by before this driver object is destructed.
  fdf::Dispatcher initial_dispatcher_ __TA_GUARDED(lock_);
};

// Extracts the default_dispatcher_opts from |program| and converts it to
// the options value expected by |fdf::Dispatcher::Create|.
// Returns zero if no options were specified.
uint32_t ExtractDefaultDispatcherOpts(const fuchsia_data::wire::Dictionary& program);

zx::result<fdf::Dispatcher> CreateDispatcher(const fbl::RefPtr<Driver>& driver,
                                             uint32_t dispatcher_opts, std::string scheduler_role);

struct LoadedDriver {
  fbl::RefPtr<Driver> driver;
  fuchsia_driver_framework::DriverStartArgs start_args;
  fdf::Dispatcher dispatcher;
};

void LoadDriver(fuchsia_driver_framework::DriverStartArgs start_args,
                async_dispatcher_t* dispatcher,
                fit::callback<void(zx::result<LoadedDriver>)> callback);

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_H_
