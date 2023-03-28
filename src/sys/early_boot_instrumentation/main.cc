// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/zx/channel.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <fbl/unique_fd.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/sys/early_boot_instrumentation/coverage_source.h"

int main(int argc, char** argv) {
  static const std::string static_dir_name(early_boot_instrumentation::kStaticDir);
  static const std::string dynamic_dir_name(early_boot_instrumentation::kDynamicDir);
  const fxl::CommandLine command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  fxl::SetLogSettingsFromCommandLine(command_line, {"early-boot-instrumentation"});

  early_boot_instrumentation::SinkDirMap sink_map;
  [&sink_map]() {
    zx::result client_end = component::Connect<fuchsia_boot::SvcStashProvider>();
    if (client_end.is_error()) {
      FX_PLOGS(ERROR, client_end.status_value())
          << "Failed to connect to "
          << fidl::DiscoverableProtocolDefaultPath<fuchsia_boot::SvcStashProvider>;
      return;
    }
    fidl::WireSyncClient client(std::move(client_end.value()));
    const fidl::WireResult result = client->Get();
    if (!result.ok()) {
      // TODO(https://fxbug.dev/124407): s/WARNING/ERROR/.
      FX_PLOGS(WARNING, result.status()) << "Failed to call fuchsia.boot/SvcStashProvider.Get";
      return;
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      FX_PLOGS(ERROR, response.error_value())
          << "Error response from fuchsia.boot/SvcStashProvider.Get";
      return;
    }
    sink_map = early_boot_instrumentation::ExtractDebugData(std::move(response->resource));
  }();

  // TODO(fxbug.dev/124317): This code does not create any directories when
  // profraw files are not available. Fix it as per contract.
  [&sink_map]() {
    fbl::unique_fd kernel_data_dir(open("/boot/kernel/data", O_RDONLY));
    if (!kernel_data_dir) {
      const char* err = strerror(errno);
      FX_LOGS(ERROR) << "Could not obtain handle to '/boot/kernel/data': " << err;
      return;
    }

    if (zx::result<> result =
            early_boot_instrumentation::ExposeKernelProfileData(kernel_data_dir, sink_map);
        result.is_error()) {
      FX_PLOGS(ERROR, result.status_value()) << "Could not expose kernel profile data";
    }
  }();

  [&sink_map]() {
    fbl::unique_fd phys_data_dir(open("/boot/kernel/data/phys", O_RDONLY));
    if (!phys_data_dir) {
      const char* err = strerror(errno);
      FX_LOGS(ERROR) << "Could not obtain handle to '/boot/kernel/data/phys': " << err;
      return;
    }

    if (zx::result<> result =
            early_boot_instrumentation::ExposePhysbootProfileData(phys_data_dir, sink_map);
        result.is_error()) {
      FX_PLOGS(ERROR, result.status_value()) << "Could not expose physboot profile data";
    }
  }();

  std::unique_ptr context = sys::ComponentContext::Create();
  vfs::PseudoDir* debug_data = context->outgoing()->GetOrCreateDirectory("debugdata");
  for (auto& [sink, root] : sink_map) {
    debug_data->AddEntry(sink, std::move(root));
  }
  sink_map.clear();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  if (zx_status_t status = context->outgoing()->ServeFromStartupInfo(loop.dispatcher());
      status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "Could not serve outgoing directory";
  }
  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "Could not run async loop";
  };
  return 0;
}
