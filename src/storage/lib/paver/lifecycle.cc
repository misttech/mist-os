// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/lifecycle.h"

#include <lib/fidl/cpp/wire/server.h>
#include <lib/syslog/cpp/macros.h>

#include "src/storage/lib/paver/pave-logging.h"

namespace paver {

void LifecycleServer::Create(async_dispatcher_t* dispatcher, ShutdownCallback shutdown,
                             fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> request) {
  if (!request.is_valid()) {
    LOG("Invalid handle for lifecycle events, assuming test environment and continuing");
    return;
  }
  fidl::BindServer(dispatcher, std::move(request),
                   std::make_unique<LifecycleServer>(std::move(shutdown)));
}

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

}  // namespace paver
