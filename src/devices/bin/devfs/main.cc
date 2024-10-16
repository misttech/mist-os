// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.process.lifecycle/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include <unordered_set>

#include "src/lib/fxl/strings/join_strings.h"

int main(int argc, const char** argv) {
  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> component_lifecycle_request(
      zx::channel(zx_take_startup_handle(PA_LIFECYCLE)));
  if (!component_lifecycle_request.is_valid()) {
    FX_LOG_KV(FATAL, "No valid handle found for lifecycle events");
  }

  if (zx::result result = component::Connect(std::move(component_lifecycle_request),
                                             "/svc/fuchsia.device.fs.lifecycle.Lifecycle");
      result.is_error()) {
    // TODO(https://fxbug.dev/42052807): Standardize status emission.
    FX_LOG_KV(FATAL, "Failed to connect to fuchsia.device.fs.lifecycle",
              FX_KV("status", result.status_string()));
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  component::OutgoingDirectory outgoing(loop.dispatcher());
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithDispatcher(loop.dispatcher()).BuildAndInitialize();

  {
    fdio_flat_namespace_t* ns;
    if (zx_status_t status = fdio_ns_export_root(&ns); status != ZX_OK) {
      // TODO(https://fxbug.dev/42052807): Standardize status emission.
      FX_LOG_KV(FATAL, "Failed to export flat namespace",
                FX_KV("status", zx_status_get_string(status)));
    }
    const fit::deferred_action cleanup = fit::defer([ns]() { fdio_ns_free_flat_ns(ns); });

    // Expose expected entries and error if any remain unexposed.
    std::unordered_set<std::string_view> expose = {"dev"};
    for (size_t i = 0; i < ns->count; ++i) {
      std::string_view path{ns->path[i]};
      // Leading slashes are not allowed.
      if (cpp20::starts_with(path, '/')) {
        path.remove_prefix(1);
      }
      if (auto nh = expose.extract(path); nh.empty()) {
        continue;
      }
      // Ensure the handle isn't closed when the namespace is freed.
      zx_handle_t handle = std::exchange(ns->handle[i], ZX_HANDLE_INVALID);
      fidl::ClientEnd<fuchsia_io::Directory> client_end{zx::channel{handle}};
      if (zx::result result = outgoing.AddDirectory(std::move(client_end), path);
          result.is_error()) {
        // TODO(https://fxbug.dev/42052807): Standardize status emission.
        FX_LOG_KV(FATAL, "Failed to expose", FX_KV("path", path),
                  FX_KV("status", result.status_string()));
      }
    }
    if (!expose.empty()) {
      const std::string missing = fxl::JoinStrings(expose, ",");
      FX_LOG_KV(FATAL, "Failed to expose all entries", FX_KV("missing", missing));
    }
  }

  if (zx::result result = outgoing.ServeFromStartupInfo(); result.is_error()) {
    // TODO(https://fxbug.dev/42052807): Standardize status emission.
    FX_LOG_KV(FATAL, "Failed to serve from startup info", FX_KV("status", result.status_string()));
  }

  FX_LOG_KV(DEBUG, "Initialized.");

  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    // TODO(https://fxbug.dev/42052807): Standardize status emission.
    FX_LOG_KV(FATAL, "Failed to run async loop", FX_KV("status", zx_status_get_string(status)));
  }

  return 0;
}
