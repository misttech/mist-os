// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/driver.h"

#include <lib/async/cpp/task.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <zircon/dlfcn.h>

#include <fbl/auto_lock.h>

#include "src/devices/lib/log/log.h"
#include "src/lib/driver_symbols/symbols.h"

namespace fdh = fuchsia_driver_host;
namespace fio = fuchsia_io;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace driver_host {

namespace {

static constexpr std::string_view kCompatDriverRelativePath = "driver/compat.so";

std::string_view GetFilename(std::string_view path) {
  size_t index = path.rfind('/');
  return index == std::string_view::npos ? path : path.substr(index + 1);
}

std::string_view GetManifest(std::string_view url) { return GetFilename(url); }

class FileEventHandler final : public fidl::AsyncEventHandler<fio::File> {
 public:
  explicit FileEventHandler(std::string url) : url_(std::move(url)) {}

  void on_fidl_error(fidl::UnbindInfo info) override {
    LOGF(ERROR, "Failed to start driver '%s'; could not open library: %s", url_.c_str(),
         info.FormatDescription().c_str());
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fio::File>) override {}

 private:
  std::string url_;
};

zx::result<fidl::ClientEnd<fio::File>> OpenDriverFile(const fdf::DriverStartArgs& start_args,
                                                      std::string_view relative_binary_path) {
  const auto& incoming = start_args.incoming();
  auto pkg = incoming ? fdf_internal::NsValue(*incoming, "/pkg") : zx::error(ZX_ERR_INVALID_ARGS);
  if (pkg.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing '/pkg' directory: %s", pkg.status_string());
    return pkg.take_error();
  }
  // Open the driver's binary within the driver's package.
  auto [client_end, server_end] = fidl::Endpoints<fio::File>::Create();
  zx_status_t status =
      fdio_open3_at(pkg->channel()->get(), relative_binary_path.data(),
                    static_cast<uint64_t>(fio::wire::kPermReadable | fio::wire::kPermExecutable),
                    server_end.TakeChannel().release());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver; could not open library: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(client_end));
}

}  // namespace

zx::result<fbl::RefPtr<Driver>> Driver::Load(std::string url, zx::vmo vmo,
                                             std::string_view relative_binary_path) {
  // Give the driver's VMO a name.
  std::string_view vmo_name = GetFilename(relative_binary_path);
  zx_status_t status = vmo.set_property(ZX_PROP_NAME, vmo_name.data(), vmo_name.size());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver '%s', could not name library VMO '%s': %s", url.c_str(),
         std::string(vmo_name).c_str(), zx_status_get_string(status));
    return zx::error(status);
  }

  // If we are using the compat shim, we do symbol validation when loading the DFv1 driver vmo.
  // This is as here the |vmo| will correspond to the compat driver, but the |url| will be the
  // DFv1 driver's url, so we would be incorrectly looking for |url| in the allowlist for
  // the compat driver's symbols.
  if (relative_binary_path != kCompatDriverRelativePath) {
    auto result = driver_symbols::FindRestrictedSymbols(vmo, url);
    if (result.is_error()) {
      LOGF(WARNING, "Driver '%s' failed to validate as ELF: %s", url.c_str(),
           result.status_value());
    } else if (result->size() > 0) {
      LOGF(ERROR, "Driver '%s' referenced %lu restricted libc symbols: ", url.c_str(),
           result->size());
      for (auto& str : *result) {
        LOGF(ERROR, str.c_str());
      }
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
  }

  void* library = dlopen_vmo(vmo.get(), RTLD_NOW);
  if (library == nullptr) {
    LOGF(ERROR, "Failed to start driver '%s', could not load library: %s", url.data(), dlerror());
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto registration =
      static_cast<const DriverRegistration*>(dlsym(library, "__fuchsia_driver_registration__"));

  if (registration == nullptr) {
    LOGF(
        ERROR,
        "__fuchsia_driver_registration__ symbol not available, falling back to __fuchsia_driver_lifecycle__.",
        url.data());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if (registration->version < 1 || registration->version > DRIVER_REGISTRATION_VERSION_MAX) {
    LOGF(ERROR, "Failed to start driver '%s', unknown driver registration version: %lu", url.data(),
         registration->version);
    return zx::error(ZX_ERR_WRONG_TYPE);
  }

  return zx::ok(fbl::MakeRefCounted<Driver>(std::move(url), library, registration));
}

Driver::Driver(std::string url, void* library, DriverHooks hooks)
    : url_(std::move(url)), library_(library), hooks_(hooks) {}

Driver::~Driver() {
  if (token_.has_value()) {
    hooks_->v1.destroy(token_.value());
  } else {
    LOGF(WARNING, "Failed to Destroy driver '%s', initialize was not completed.", url_.c_str());
  }

  dlclose(library_);
}

void Driver::set_binding(fidl::ServerBindingRef<fdh::Driver> binding) {
  fbl::AutoLock al(&lock_);
  binding_.emplace(std::move(binding));
}

void Driver::Stop(StopCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  if (driver_client_.has_value()) {
    driver_client_.value().AsyncCall(&DriverClient::Stop);
  } else {
    LOGF(ERROR, "The driver_client_ is not available.");
  }
}

void Driver::Start(fbl::RefPtr<Driver> self, fuchsia_driver_framework::DriverStartArgs start_args,
                   ::fdf::Dispatcher dispatcher, fit::callback<void(zx::result<>)> cb) {
  fbl::AutoLock al(&lock_);
  initial_dispatcher_ = std::move(dispatcher);

  zx::result endpoints = fdf::CreateEndpoints<fuchsia_driver_framework::Driver>();
  if (endpoints.is_error()) {
    cb(endpoints.take_error());
    return;
  }

  async::PostTask(initial_dispatcher_.async_dispatcher(),
                  [this, hooks = hooks_, server = std::move(endpoints->server)]() mutable {
                    void* token = hooks->v1.initialize(server.TakeHandle().release());
                    fbl::AutoLock al(&lock_);
                    token_.emplace(token);
                  });

  zx::result client_dispatcher_result = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      this, {}, "fdf-driver-client-dispatcher",
      [](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });

  ZX_ASSERT_MSG(client_dispatcher_result.is_ok(), "Failed to create driver client dispatcher: %s",
                client_dispatcher_result.status_string());

  client_dispatcher_ = std::move(client_dispatcher_result.value());
  driver_client_.emplace(client_dispatcher_.async_dispatcher(), std::in_place, self, url_);
  driver_client_.value().AsyncCall(&DriverClient::Bind, std::move(endpoints->client));
  driver_client_.value().AsyncCall(&DriverClient::Start, std::move(start_args), std::move(cb));
}

void Driver::ShutdownClient() {
  fbl::AutoLock al(&lock_);
  driver_client_.reset();
  client_dispatcher_.ShutdownAsync();
  // client_dispatcher_ will destroy itself in the shutdown completion callback.
  client_dispatcher_.release();
}

void Driver::Unbind() {
  fbl::AutoLock al(&lock_);
  if (binding_.has_value()) {
    // The binding's unbind handler will begin shutting down all dispatchers belonging to this
    // driver and when that is complete, it will remove this driver from its list causing this to
    // destruct.
    binding_->Unbind();

    // ServerBindingRef does not have ownership so resetting this is fine to avoid calling Unbind
    // multiple times.
    binding_.reset();
  }
}

uint32_t ExtractDefaultDispatcherOpts(const fuchsia_data::wire::Dictionary& program) {
  auto default_dispatcher_opts =
      fdf_internal::ProgramValueAsVector(program, "default_dispatcher_opts");

  uint32_t opts = 0;
  if (default_dispatcher_opts.is_ok()) {
    for (const auto& opt : *default_dispatcher_opts) {
      if (opt == "allow_sync_calls") {
        opts |= FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS;
      } else {
        LOGF(WARNING, "Ignoring unknown default_dispatcher_opt: %s", opt.c_str());
      }
    }
  }
  return opts;
}

zx::result<fdf::Dispatcher> CreateDispatcher(const fbl::RefPtr<Driver>& driver,
                                             uint32_t dispatcher_opts, std::string scheduler_role) {
  auto name = GetManifest(driver->url());
  // The dispatcher must be shutdown before the dispatcher is destroyed.
  // Usually we will wait for the callback from |fdf_env::DriverShutdown| before destroying
  // the driver object (and hence the dispatcher).
  // In the case where we fail to start the driver, the driver object would be destructed
  // immediately, so here we hold an extra reference to the driver object to ensure the
  // dispatcher will not be destructed until shutdown completes.
  //
  // We do not destroy the dispatcher in the shutdown callback, to prevent crashes that
  // would happen if the driver attempts to access the dispatcher in its Stop hook.
  //
  // Currently we only support synchronized dispatchers for the default dispatcher.
  return fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      driver.get(), fdf::SynchronizedDispatcher::Options{.value = dispatcher_opts},
      std::format("{}-default-{:p}", name, static_cast<void*>(driver.get())),
      [driver_ref = driver](fdf_dispatcher_t* dispatcher) {}, scheduler_role);
}

void LoadDriver(fuchsia_driver_framework::DriverStartArgs start_args,
                async_dispatcher_t* dispatcher,
                fit::callback<void(zx::result<LoadedDriver>)> callback) {
  if (!start_args.url()) {
    LOGF(ERROR, "Failed to start driver, missing 'url' argument");
    callback(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  if (!start_args.program().has_value()) {
    LOGF(ERROR, "Failed to start driver, missing 'program' argument");
    callback(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  const std::string& url = *start_args.url();
  fidl::Arena arena;
  fuchsia_data::wire::Dictionary wire_program = fidl::ToWire(arena, *start_args.program());

  zx::result<std::string> binary = fdf_internal::ProgramValue(wire_program, "binary");
  if (binary.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing 'binary' argument: %s", binary.status_string());
    callback(binary.take_error());
    return;
  }

  auto driver_file = OpenDriverFile(start_args, *binary);
  if (driver_file.is_error()) {
    LOGF(ERROR, "Failed to open driver '%s' file: %s", url.c_str(), driver_file.status_string());
    callback(driver_file.take_error());
    return;
  }

  uint32_t default_dispatcher_opts = driver_host::ExtractDefaultDispatcherOpts(wire_program);
  std::string default_dispatcher_scheduler_role = "";
  {
    auto scheduler_role =
        fdf_internal::ProgramValue(wire_program, "default_dispatcher_scheduler_role");
    if (scheduler_role.is_ok()) {
      default_dispatcher_scheduler_role = *scheduler_role;
    } else if (scheduler_role.status_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "Failed to parse scheduler role: %s", scheduler_role.status_string());
    }
  }

  // Once we receive the VMO from the call to GetBackingMemory, we can load the driver into this
  // driver host. We move the storage and encoded for start_args into this callback to extend its
  // lifetime.
  fidl::SharedClient file(std::move(*driver_file), dispatcher,
                          std::make_unique<FileEventHandler>(url));
  auto vmo_callback = [start_args = std::move(start_args), default_dispatcher_opts,
                       default_dispatcher_scheduler_role, callback = std::move(callback),
                       relative_binary_path = *binary, _ = file.Clone()](
                          fidl::Result<fio::File::GetBackingMemory>& result) mutable {
    const std::string& url = *start_args.url();
    if (!result.is_ok()) {
      LOGF(ERROR, "Failed to start driver '%s', could not get library VMO: %s", url.c_str(),
           result.error_value().FormatDescription().c_str());
      zx_status_t status = result.error_value().is_domain_error()
                               ? result.error_value().domain_error()
                               : result.error_value().framework_error().status();
      callback(zx::error(status));
      return;
    }
    auto driver = Driver::Load(url, std::move(result->vmo()), relative_binary_path);
    if (driver.is_error()) {
      LOGF(ERROR, "Failed to start driver '%s', could not Load driver: %s", url.c_str(),
           driver.status_string());
      callback(driver.take_error());
      return;
    }

    zx::result<fdf::Dispatcher> driver_dispatcher =
        CreateDispatcher(*driver, default_dispatcher_opts, default_dispatcher_scheduler_role);
    if (driver_dispatcher.is_error()) {
      LOGF(ERROR, "Failed to start driver '%s', could not create dispatcher: %s", url.c_str(),
           driver_dispatcher.status_string());
      callback(driver_dispatcher.take_error());
      return;
    }

    callback(zx::ok(LoadedDriver{
        .driver = std::move(*driver),
        .start_args = std::move(start_args),
        .dispatcher = std::move(*driver_dispatcher),
    }));
  };
  file->GetBackingMemory(fio::VmoFlags::kRead | fio::VmoFlags::kExecute |
                         fio::VmoFlags::kPrivateClone)
      .ThenExactlyOnce(std::move(vmo_callback));
}

}  // namespace driver_host
