// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/paver.h"

#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/server.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/astro.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/nelson.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/vim3.h"
#include "src/storage/lib/paver/violet.h"
#include "src/storage/lib/paver/x64.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

class LifecycleServer final : public fidl::WireServer<fuchsia_process_lifecycle::Lifecycle> {
 public:
  using FinishedCallback = fit::callback<void(zx_status_t status)>;
  using ShutdownCallback = fit::callback<void(FinishedCallback)>;

  explicit LifecycleServer(ShutdownCallback shutdown) : shutdown_(std::move(shutdown)) {}

  void Stop(StopCompleter::Sync& completer) override;

 private:
  ShutdownCallback shutdown_;
};

void LifecycleServer::Stop(StopCompleter::Sync& completer) {
  LOG("Received shutdown command over lifecycle interface");
  shutdown_([completer = completer.ToAsync()](zx_status_t status) mutable {
    if (status != ZX_OK) {
      ERROR("Shutdown failed: %s", zx_status_get_string(status));
    } else {
      LOG("Paver shutdown complete");
    }
    completer.Close(status);
  });
}

int main(int argc, char** argv) {
  zx_status_t status = StdoutToDebuglog::Init();
  if (status != ZX_OK) {
    LOG("Failed to redirect stdout to debuglog, assuming test environment and continuing\n");
  }
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  component::OutgoingDirectory outgoing(dispatcher);

  paver::Paver paver;
  paver.set_dispatcher(dispatcher);

  // NOTE: Ordering matters!
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AstroPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::NelsonPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::SherlockPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::LuisPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::Vim3PartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::VioletPartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::X64PartitionerFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::DefaultPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AstroAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::NelsonAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::SherlockAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::LuisAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::Vim3AbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::VioletAbrClientFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::X64AbrClientFactory>());

  fidl::ServerBindingGroup<fuchsia_paver::Paver> bindings;
  zx::result result = outgoing.AddUnmanagedProtocol<fuchsia_paver::Paver>(
      bindings.CreateHandler(&paver, dispatcher, fidl::kIgnoreBindingClosure));
  if (result.is_error()) {
    ERROR("paver: error: Failed add paver protocol: %s.\n", result.status_string());
    return 1;
  }

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    ERROR("paver: error: Failed to serve outgoing directory: %s.\n", result.status_string());
    return 1;
  }

  zx::channel lifecycle_channel = zx::channel(zx_take_startup_handle(PA_LIFECYCLE));
  if (!lifecycle_channel.is_valid()) {
    ERROR("PA_LIFECYCLE startup handle is required.");
    return 1;
  }
  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> lifecycle_request(
      std::move(lifecycle_channel));

  LifecycleServer lifecycle(fit::bind_member<&paver::Paver::LifecycleStopCallback>(&paver));
  fidl::ServerBinding lifecycle_binding(dispatcher, std::move(lifecycle_request), &lifecycle,
                                        fidl::kIgnoreBindingClosure);
  if (result.is_error()) {
    ERROR("paver: error: Failed add paver protocol: %s.\n", result.status_string());
    return 1;
  }

  return loop.Run();
}
