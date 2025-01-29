// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_DRIVER_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_DRIVER_H_

#include <fidl/fuchsia.driver.host/cpp/fidl.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <unordered_map>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include "src/devices/bin/driver_host/driver_client.h"

namespace driver_host {

using DriverHooks = const DriverRegistration*;

using OverrideMap = std::unordered_map<std::string, fidl::ClientEnd<fuchsia_io::File>>;

struct Module {
  zx::vmo module_vmo;
  OverrideMap overrides;
  std::vector<std::string> symbols;
};

using ModuleMap = std::unordered_map<std::string, Module>;

struct Symbol {
  std::string module_name;
  std::string symbol_name;
  void* address;
};

class Driver : public fidl::Server<fuchsia_driver_host::Driver>,
               public fbl::RefCounted<Driver>,
               public fbl::DoublyLinkedListable<fbl::RefPtr<Driver>> {
 public:
  static zx::result<fbl::RefPtr<Driver>> Load(std::string url, zx::vmo vmo,
                                              std::string_view relative_binary_path,
                                              ModuleMap modules);

  Driver(std::string url, void* library, std::vector<void*> modules, std::vector<Symbol> symbols,
         DriverHooks hooks);
  ~Driver() override;

  const std::string& url() const { return url_; }
  void set_binding(fidl::ServerBindingRef<fuchsia_driver_host::Driver> binding)
      __TA_EXCLUDES(lock_);

  void Stop(StopCompleter::Sync& completer) override;

  // Starts the driver.
  void Start(fbl::RefPtr<Driver> self, fuchsia_driver_framework::DriverStartArgs start_args,
             fdf::Dispatcher dispatcher, fit::callback<void(zx::result<>)> cb) __TA_EXCLUDES(lock_);

  // Resets the driver_client_ and begin shutting down the client_dispatcher_.
  // Releases ownership of the client_dispatcher.
  // The dispatcher will destroy itself once its shutdown has completed.
  // This may be called from the context of the |client_dispatcher_|.
  void ShutdownClient() __TA_EXCLUDES(lock_);

  // Unbind the driver's `fuchsia_driver_host::Driver` binding. This will run the unbind handler
  // that the driver host setup for us, which will initiate the dispatcher shutdown, after which
  // this driver will be removed from the drivers list and destructed.
  // This may be called from the context of the |client_dispatcher_|.
  void Unbind() __TA_EXCLUDES(lock_);

 private:
  std::string url_;
  void* library_;
  std::vector<void*> modules_;
  std::vector<Symbol> symbols_;

  fbl::Mutex lock_;

  // The hooks to initialize and destroy the driver. Backed by the registration symbol.
  DriverHooks hooks_ __TA_GUARDED(lock_);

  // The binding is set from the driver_host using |set_binding|.
  std::optional<fidl::ServerBindingRef<fuchsia_driver_host::Driver>> binding_ __TA_GUARDED(lock_);

  // The initial dispatcher of the driver.
  // This is where the initialize hook is called for the driver.
  fdf::Dispatcher initial_dispatcher_ __TA_GUARDED(lock_);

  // This is the dispatcher used by this class for the driver_client_ object.
  fdf::SynchronizedDispatcher client_dispatcher_ __TA_GUARDED(lock_);

  // This is a wrapper for the client to the driver, it's bound to |client_dispatcher_|.
  std::optional<async_patterns::DispatcherBound<DriverClient>> driver_client_ __TA_GUARDED(lock_);

  // This is set through the initialize hook and passed into destroy.
  std::optional<void*> token_ __TA_GUARDED(lock_);
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

}  // namespace driver_host

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_DRIVER_H_
