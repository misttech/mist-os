// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/paver.h"

#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/server.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

#include "src/storage/lib/paver/abr-client.h"
#include "src/storage/lib/paver/device-partitioner.h"
#include "src/storage/lib/paver/pave-logging.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

#if defined(LEGACY_PAVER)
#include "src/storage/lib/paver/android.h"
#include "src/storage/lib/paver/astro.h"
#include "src/storage/lib/paver/luis.h"
#include "src/storage/lib/paver/nelson.h"
#include "src/storage/lib/paver/sherlock.h"
#include "src/storage/lib/paver/vim3.h"
#include "src/storage/lib/paver/violet.h"
#include "src/storage/lib/paver/x64.h"
#elif defined(astro)
#include "src/storage/lib/paver/astro.h"
#elif defined(luis)
#include "src/storage/lib/paver/luis.h"
#elif defined(nelson)
#include "src/storage/lib/paver/nelson.h"
#elif defined(sherlock)
#include "src/storage/lib/paver/sherlock.h"
#elif defined(vim3)
#include "src/storage/lib/paver/vim3.h"
#elif defined(violet)
#include "src/storage/lib/paver/violet.h"
#elif defined(x64)
#include "src/storage/lib/paver/x64.h"
#elif defined(android)
#include "src/storage/lib/paver/android.h"
#endif

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

#if defined(LEGACY_PAVER)
  // NOTE: Ordering matters!
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AstroPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AstroAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::NelsonPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::NelsonAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::SherlockPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::SherlockAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::LuisPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::LuisAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::Vim3PartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::Vim3AbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::VioletPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::VioletAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AndroidPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AndroidAbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::X64PartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::X64AbrClientFactory>());
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::DefaultPartitionerFactory>());
#elif defined(astro)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AstroPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AstroAbrClientFactory>());
#elif defined(nelson)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::NelsonPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::NelsonAbrClientFactory>());
#elif defined(sherlock)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::SherlockPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::SherlockAbrClientFactory>());
#elif defined(luis)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::LuisPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::LuisAbrClientFactory>());
#elif defined(vim3)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::Vim3PartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::Vim3AbrClientFactory>());
#elif defined(violet)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::VioletPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::VioletAbrClientFactory>());
#elif defined(x64)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::X64PartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::X64AbrClientFactory>());
#elif defined(android)
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::AndroidPartitionerFactory>());
  abr::ClientFactory::Register(std::make_unique<paver::AndroidAbrClientFactory>());
#else
  paver::DevicePartitionerFactory::Register(std::make_unique<paver::DefaultPartitionerFactory>());
#endif

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
