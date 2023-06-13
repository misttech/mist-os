// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host2/driver.h"

#include <lib/async/cpp/task.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <zircon/dlfcn.h>

#include <fbl/auto_lock.h>
#include <fbl/string_printf.h>

#include "src/devices/lib/log/log.h"

namespace fdh = fuchsia_driver_host;
namespace fio = fuchsia_io;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace dfv2 {

namespace {

std::string_view GetManifest(std::string_view url) {
  auto index = url.rfind('/');
  return index == std::string_view::npos ? url : url.substr(index + 1);
}

class FileEventHandler : public fidl::AsyncEventHandler<fio::File> {
 public:
  explicit FileEventHandler(std::string url) : url_(std::move(url)) {}

  void on_fidl_error(fidl::UnbindInfo info) override {
    LOGF(ERROR, "Failed to start driver '%s'; could not open library: %s", url_.c_str(),
         info.FormatDescription().c_str());
  }

 private:
  std::string url_;
};

zx::result<fidl::ClientEnd<fio::File>> OpenDriverFile(
    const fdf::DriverStartArgs& start_args, const fuchsia_data::wire::Dictionary& program) {
  const auto& incoming = start_args.incoming();
  auto pkg = incoming ? fdf_internal::NsValue(*incoming, "/pkg") : zx::error(ZX_ERR_INVALID_ARGS);
  if (pkg.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing '/pkg' directory: %s", pkg.status_string());
    return pkg.take_error();
  }

  zx::result<std::string> binary = fdf_internal::ProgramValue(program, "binary");
  if (binary.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing 'binary' argument: %s", binary.status_string());
    return binary.take_error();
  }
  // Open the driver's binary within the driver's package.
  auto endpoints = fidl::CreateEndpoints<fio::File>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  zx_status_t status = fdio_open_at(
      pkg->channel()->get(), binary->data(),
      static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightExecutable),
      endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver; could not open library: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(endpoints->client));
}

}  // namespace

zx::result<fbl::RefPtr<Driver>> Driver::Load(std::string url, zx::vmo vmo) {
  // Give the driver's VMO a name. We can't fit the entire URL in the name, so
  // use the name of the manifest from the URL.
  auto manifest = GetManifest(url);
  zx_status_t status = vmo.set_property(ZX_PROP_NAME, manifest.data(), manifest.size());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver '%s',, could not name library VMO: %s", url.c_str(),
         zx_status_get_string(status));
    return zx::error(status);
  }

  void* library = dlopen_vmo(vmo.get(), RTLD_NOW);
  if (library == nullptr) {
    LOGF(ERROR, "Failed to start driver '%s', could not load library: %s", url.data(), dlerror());
    return zx::error(ZX_ERR_INTERNAL);
  }
  auto lifecycle =
      static_cast<const DriverLifecycle*>(dlsym(library, "__fuchsia_driver_lifecycle__"));
  if (lifecycle == nullptr) {
    LOGF(ERROR, "Failed to start driver '%s', driver lifecycle not found", url.data());
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  if (lifecycle->version < 1 || lifecycle->version > DRIVER_LIFECYCLE_VERSION_MAX) {
    LOGF(ERROR, "Failed to start driver '%s', unknown driver lifecycle version: %lu", url.data(),
         lifecycle->version);
    return zx::error(ZX_ERR_WRONG_TYPE);
  }
  return zx::ok(fbl::MakeRefCounted<Driver>(std::move(url), library, lifecycle));
}

Driver::Driver(std::string url, void* library, const DriverLifecycle* lifecycle)
    : url_(std::move(url)), library_(library), lifecycle_(lifecycle) {}

Driver::~Driver() {
  fbl::AutoLock al(&lock_);
  if (opaque_.has_value()) {
    void* opaque = *opaque_;
    al.release();
    zx_status_t status = lifecycle_->v1.stop(opaque);
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to stop driver '%s': %s", url_.data(), zx_status_get_string(status));
    }
  } else {
    al.release();
  }
  dlclose(library_);
}

void Driver::set_binding(fidl::ServerBindingRef<fdh::Driver> binding) {
  fbl::AutoLock al(&lock_);
  binding_.emplace(std::move(binding));
}

void Driver::Stop(StopCompleter::Sync& completer) {
  // Prepare stop was added in version 2.
  if (lifecycle_->version >= 2 && lifecycle_->v2.prepare_stop != nullptr) {
    // We synchronize this task with start by posting it against the dispatcher used in Start.
    async_dispatcher_t* dispatcher;
    {
      fbl::AutoLock al(&lock_);
      dispatcher = initial_dispatcher_.async_dispatcher();
    }
    zx_status_t status = async::PostTask(dispatcher, [this]() {
      void* opaque_driver;
      {
        fbl::AutoLock al(&lock_);
        ZX_ASSERT(opaque_.has_value());
        opaque_driver = opaque_.value();
      }
      lifecycle_->v2.prepare_stop(
          opaque_driver,
          [](void* cookie, zx_status_t status) {
            static_cast<Driver*>(cookie)->PrepareStopCompleted(status);
          },
          this);
    });
    // It shouldn't be possible for this to fail as the dispatcher shouldn't be shutdown by anyone
    // other than the driver host.
    ZX_ASSERT(status == ZX_OK);
  } else {
    fbl::AutoLock al(&lock_);
    binding_->Unbind();
  }
}

void Driver::PrepareStopCompleted(zx_status_t status) {
  if (status != ZX_OK) {
    LOGF(ERROR, "prepare_stop failed with status: %s", zx_status_get_string(status));
  }
  {
    fbl::AutoLock al(&lock_);
    if (!binding_.has_value()) {
      LOGF(ERROR, "Driver::binding_ does not exist.");
      return;
    }

    binding_->Unbind();
  }
}

void Driver::StartCompleted(zx_status_t status, void* opaque) {
  // Note: May be called from a random thread context, before `lifecycle_->v3.start` returns.
  fit::callback<void(zx::result<>)> cb;
  {
    fbl::AutoLock al(&lock_);
    cb = std::move(start_callback_);
    if (!cb) {
      LOGF(ERROR, "Start completed multiple times.");
      return;
    }
    opaque_.emplace(opaque);
  }

  cb(zx::make_result(status));
}

void Driver::Start(fuchsia_driver_framework::DriverStartArgs start_args,
                   ::fdf::Dispatcher dispatcher, fit::callback<void(zx::result<>)> cb) {
  fdf_dispatcher_t* initial_dispatcher = dispatcher.get();
  {
    fbl::AutoLock al(&lock_);
    initial_dispatcher_ = std::move(dispatcher);
  }

  fidl::OwnedEncodeResult encoded = fidl::StandaloneEncode(std::move(start_args));
  if (!encoded.message().ok()) {
    LOGF(ERROR, "Failed to start driver, could not encode start args: %s",
         encoded.message().FormatDescription().data());
    cb(zx::error(encoded.message().status()));
    return;
  }
  fidl_opaque_wire_format_metadata_t wire_format_metadata =
      encoded.wire_format_metadata().ToOpaque();

  // We convert the outgoing message into an incoming message to provide to the
  // driver on start.
  fidl::OutgoingToEncodedMessage converted_message{encoded.message()};
  if (!converted_message.ok()) {
    LOGF(ERROR, "Failed to start driver, could not convert start args: %s",
         converted_message.FormatDescription().data());
    cb(zx::error(converted_message.status()));
    return;
  }

  // After calling |lifecycle_->start|, we assume it has taken ownership of
  // the handles from |start_args|, and can therefore relinquish ownership.
  fidl_incoming_msg_t c_msg = std::move(converted_message.message()).ReleaseToEncodedCMessage();
  EncodedFidlMessage msg{
      .bytes = static_cast<uint8_t*>(c_msg.bytes),
      .handles = c_msg.handles,
      .num_bytes = c_msg.num_bytes,
      .num_handles = c_msg.num_handles,
  };
  void* opaque = nullptr;

  // Async start was added in version 3.
  if (lifecycle_->version >= 3 && lifecycle_->v3.start != nullptr) {
    {
      fbl::AutoLock al(&lock_);
      start_callback_ = std::move(cb);
    }
    lifecycle_->v3.start(
        {msg, wire_format_metadata}, initial_dispatcher,
        [](void* cookie, zx_status_t status, void* opaque) {
          static_cast<Driver*>(cookie)->StartCompleted(status, opaque);
        },
        this);

  } else {
    zx_status_t status =
        lifecycle_->v1.start({msg, wire_format_metadata}, initial_dispatcher, &opaque);
    if (status != ZX_OK) {
      cb(zx::error(status));
      return;
    }
    cb(zx::ok());
    {
      fbl::AutoLock al(&lock_);
      opaque_.emplace(opaque);
    }
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
      fbl::StringPrintf("%.*s-default-%p", static_cast<int>(name.size()), name.data(),
                        driver.get()),
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

  auto driver_file = OpenDriverFile(start_args, wire_program);
  if (driver_file.is_error()) {
    LOGF(ERROR, "Failed to open driver '%s' file: %s", url.c_str(), driver_file.status_string());
    callback(driver_file.take_error());
    return;
  }

  uint32_t default_dispatcher_opts = dfv2::ExtractDefaultDispatcherOpts(wire_program);
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
  auto vmo_callback =
      [start_args = std::move(start_args), default_dispatcher_opts,
       default_dispatcher_scheduler_role, callback = std::move(callback),
       _ = file.Clone()](fidl::Result<fio::File::GetBackingMemory>& result) mutable {
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
        auto driver = Driver::Load(url, std::move(result->vmo()));
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

}  // namespace dfv2
