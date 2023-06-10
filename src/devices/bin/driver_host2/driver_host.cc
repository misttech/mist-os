// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host2/driver_host.h"

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/component/cpp/start_args.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/dlfcn.h>

#include "src/lib/storage/vfs/cpp/service.h"

// The driver runtime libraries use the fdf namespace, but we would also like to use fdf
// as an alias for the fdf FIDL library.
namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace fdh = fuchsia_driver_host;

namespace dfv2 {

DriverHost::DriverHost(inspect::Inspector& inspector, async::Loop& loop) : loop_(loop) {
  inspector.GetRoot().CreateLazyNode(
      "drivers", [this] { return Inspect(); }, &inspector);
}

fpromise::promise<inspect::Inspector> DriverHost::Inspect() {
  inspect::Inspector inspector;
  auto& root = inspector.GetRoot();
  size_t i = 0;

  std::lock_guard<std::mutex> lock(mutex_);
  for (auto& driver : drivers_) {
    auto child = root.CreateChild("driver-" + std::to_string(++i));
    child.CreateString("url", driver.url(), &inspector);
    inspector.emplace(std::move(child));
  }

  return fpromise::make_ok_promise(std::move(inspector));
}

zx::result<> DriverHost::PublishDriverHost(component::OutgoingDirectory& outgoing_directory) {
  const auto service = [this](fidl::ServerEnd<fdh::DriverHost> request) {
    fidl::BindServer(loop_.dispatcher(), std::move(request), this);
  };
  auto status = outgoing_directory.AddUnmanagedProtocol<fdh::DriverHost>(std::move(service));
  if (status.is_error()) {
    FX_SLOG(ERROR, "Failed to add directory entry",
            KV("name", fidl::DiscoverableProtocolName<fdh::DriverHost>),
            KV("status_str", status.status_string()));
  }

  return status;
}

void DriverHost::Start(StartRequest& request, StartCompleter::Sync& completer) {
  auto callback = [this, request = std::move(request.driver()),
                   completer = completer.ToAsync()](zx::result<LoadedDriver> loaded) mutable {
    if (loaded.is_error()) {
      completer.Reply(loaded.take_error());
      return;
    }
    async_dispatcher_t* driver_async_dispatcher = loaded->dispatcher.async_dispatcher();

    // Task to start the driver. Post this to the driver dispatcher thread.
    auto start_task = [this, request = std::move(request), completer = std::move(completer),
                       loaded = std::move(*loaded)]() mutable {
      StartDriver(std::move(loaded.driver), std::move(loaded.start_args),
                  std::move(loaded.dispatcher), std::move(request),
                  [completer = std::move(completer)](zx::result<> status) mutable {
                    completer.Reply(status);
                  });
    };
    async::PostTask(driver_async_dispatcher, std::move(start_task));
  };
  LoadDriver(std::move(request.start_args()), loop_.dispatcher(), std::move(callback));
}

void DriverHost::GetProcessInfo(GetProcessInfoCompleter::Sync& completer) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx::process::self()->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_SLOG(ERROR, "Failed to get info about process handle",
            KV("status_str", zx_status_get_string(status)));
    completer.Reply(zx::error(status));
    return;
  }
  uint64_t process_koid = info.koid;

  status =
      zx::job::default_job()->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_SLOG(ERROR, "Failed to get info about job handle",
            KV("status_str", zx_status_get_string(status)));
    completer.Reply(zx::error(status));
    return;
  }
  uint64_t job_koid = info.koid;

  completer.Reply(zx::ok(fuchsia_driver_host::ProcessInfo{{
      .job_koid = job_koid,
      .process_koid = process_koid,
  }}));
}

void DriverHost::InstallLoader(InstallLoaderRequest& request,
                               InstallLoaderCompleter::Sync& completer) {
  zx::handle old_handle(dl_set_loader_service(request.loader().TakeChannel().release()));
}

void DriverHost::StartDriver(fbl::RefPtr<Driver> driver,
                             fuchsia_driver_framework::DriverStartArgs start_args,
                             fdf::Dispatcher dispatcher,
                             fidl::ServerEnd<fuchsia_driver_host::Driver> request,
                             fit::callback<void(zx::result<>)> cb) {
  // We have to add the driver to this list before calling Start in order to have an accurate
  // count of how many drivers exist in this driver host.
  {
    std::lock_guard<std::mutex> lock(mutex_);
    drivers_.push_back(driver);
  }
  auto start_callback = [this, driver, cb = std::move(cb),
                         request = std::move(request)](zx::result<> status) mutable {
    // Note: May be called from a random thread context, before `driver->Start` returns.
    auto remove_driver = fit::defer([this, driver = driver.get()]() {
      std::lock_guard<std::mutex> lock(mutex_);
      drivers_.erase(*driver);
    });

    if (status.is_error()) {
      FX_SLOG(ERROR, "Failed to start driver", KV("url", driver->url().data()),
              KV("status_str", status.status_string()));
      // If we fail to start the driver, we need to initiate shutting down the
      // dispatcher.
      driver->ShutdownDispatcher();
      // The dispatcher will be destroyed in the shutdown callback, when the last
      // driver reference is released.
      cb(status);
      return;
    }
    cb(zx::ok());

    FX_SLOG(INFO, "Started driver", KV("url", driver->url().data()));
    auto unbind_callback = [this](Driver* driver, fidl::UnbindInfo info,
                                  fidl::ServerEnd<fdh::Driver> server) {
      if (!info.is_user_initiated()) {
        FX_SLOG(WARNING, "Unexpected stop of driver", KV("url", driver->url().data()),
                KV("status_str", info.FormatDescription()).data());
      }
      ShutdownDriver(driver, std::move(server));
    };
    auto binding = fidl::BindServer(loop_.dispatcher(), std::move(request), driver.get(),
                                    std::move(unbind_callback));
    driver->set_binding(std::move(binding));
    remove_driver.cancel();
  };
  driver->Start(std::move(start_args), std::move(dispatcher), std::move(start_callback));
}

void DriverHost::ShutdownDriver(Driver* driver, fidl::ServerEnd<fdh::Driver> server) {
  // Request the driver runtime shutdown all dispatchers owned by the driver.
  // Once we get the callback, we will stop the driver.
  auto driver_shutdown = std::make_unique<fdf_env::DriverShutdown>();
  auto driver_shutdown_ptr = driver_shutdown.get();
  auto shutdown_callback = [this, driver_shutdown = std::move(driver_shutdown), driver,
                            server = std::move(server)](const void* shutdown_driver) mutable {
    ZX_ASSERT(driver == shutdown_driver);

    std::lock_guard<std::mutex> lock(mutex_);
    // This removes the driver's unique_ptr from the list, which will
    // run the destructor and call the driver's Stop hook.
    drivers_.erase(*driver);

    // Send the epitaph to the driver runner letting it know we stopped
    // the driver correctly.
    server.Close(ZX_OK);

    // If this is the last driver, shutdown the driver host.
    if (drivers_.is_empty()) {
      // We only exit if we're not shutting down in order to match DFv1 behavior.
      // TODO(http://fxbug.dev/124305): We should always exit driver hosts when we get down to 0
      // drivers.
      zx::result client = component::Connect<fuchsia_device_manager::SystemStateTransition>();
      ZX_ASSERT_MSG(!client.is_error(), "Failed to connect to SystemStateTransition: %s",
                    client.status_string());
      fidl::WireResult result = fidl::WireCall(client.value())->GetTerminationSystemState();
      if (result.ok() == false ||
          result->state == fuchsia_device_manager::SystemPowerState::kFullyOn) {
        loop_.Quit();
      }
    }
  };
  // We always expect this call to succeed, as we should be the only entity
  // that attempts to forcibly shutdown drivers.
  ZX_ASSERT(ZX_OK == driver_shutdown_ptr->Begin(driver, std::move(shutdown_callback)));
}

}  // namespace dfv2
