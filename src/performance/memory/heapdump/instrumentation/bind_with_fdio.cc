// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.memory.heapdump.process/cpp/wire.h>
#include <lib/fdio/directory.h>

#include "heapdump/bind.h"

static constexpr const char *kServicePath =
    fidl::DiscoverableProtocolDefaultPath<fuchsia_memory_heapdump_process::Registry>;

__EXPORT void heapdump_bind_with_fdio(void) {
  zx::channel local, remote;
  auto status = zx::make_result(zx::channel::create(0, &local, &remote));
  ZX_ASSERT_MSG(status.is_ok(), "failed to create channel: %s", status.status_string());

  if (fdio_service_connect(kServicePath, remote.release()) == ZX_OK) {
    heapdump_bind_with_channel(local.release());
  } else {
    heapdump_bind_with_channel(ZX_HANDLE_INVALID);
  }
}
