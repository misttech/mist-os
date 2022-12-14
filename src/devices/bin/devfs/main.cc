// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.process.lifecycle/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/service_client.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> component_lifecycle_request(
      zx::channel(zx_take_startup_handle(PA_LIFECYCLE)));
  if (!component_lifecycle_request.is_valid()) {
    FX_SLOG(ERROR, "No valid handle found for lifecycle events");
    return -1;
  }

  auto status = component::Connect(std::move(component_lifecycle_request),
                                   "/svc/fuchsia.device.fs.lifecycle.Lifecycle");
  if (status.is_error()) {
    FX_SLOG(ERROR, "Failed to connect to fuchsia.device.fs.lifecycle",
            KV("status", status.status_value()));
    return status.status_value();
  }

  component::OutgoingDirectory outgoing(loop.dispatcher());

  auto dev_client = component::Connect<fuchsia_io::Directory>("/dev");
  if (dev_client.is_error()) {
    FX_SLOG(ERROR, "Failed to connect to /dev", KV("status", dev_client.status_value()));
    return dev_client.status_value();
  }

  zx::result result = outgoing.AddDirectory(std::move(*dev_client), "dev");
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  result = outgoing.ServeFromStartupInfo();
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  FX_SLOG(DEBUG, "Initialized.");

  loop.Run();
  return 0;
}
