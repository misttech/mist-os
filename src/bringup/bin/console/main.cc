// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.logger/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "src/bringup/bin/console/console.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

namespace {

zx::resource GetDebugResource() {
  auto client_end = component::Connect<fuchsia_kernel::DebugResource>();
  if (client_end.is_error()) {
    printf("console: Could not connect to DebugResource service: %s\n", client_end.status_string());
    return {};
  }

  fidl::WireSyncClient client{std::move(client_end.value())};
  auto result = client->Get();
  if (result.status() != ZX_OK) {
    printf("console: Could not retrieve DebugResource: %s\n",
           zx_status_get_string(result.status()));
    return {};
  }
  return std::move(result->resource);
}

}  // namespace

int main(int argc, const char** argv) {
  if (zx_status_t status = StdoutToDebuglog::Init(); status != ZX_OK) {
    printf("console: StdoutToDebuglog::Init() = %s\n", zx_status_get_string(status));
    return status;
  }

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  // Provide a RxSource that grabs the data from the kernel serial connection
  Console::RxSource rx_source = [debug_resource = GetDebugResource()](uint8_t* byte) {
    size_t length = 0;
    zx_status_t status =
        zx_debug_read(debug_resource.get(), reinterpret_cast<char*>(byte), sizeof(*byte), &length);
    if (status == ZX_ERR_NOT_SUPPORTED) {
      // Suppress the error print in this case.  No console on this machine.
      return status;
    }
    if (status != ZX_OK) {
      printf("console: error %s, length %zu from zx_debug_read syscall, exiting.\n",
             zx_status_get_string(status), length);
      return status;
    }
    if (length != 1) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return ZX_OK;
  };
  Console::TxSink tx_sink = [](const uint8_t* buffer, size_t length) {
    return zx_debug_write(reinterpret_cast<const char*>(buffer), length);
  };
  zx::eventpair event1, event2;
  if (zx_status_t status = zx::eventpair::create(0, &event1, &event2); status != ZX_OK) {
    printf("console: zx::eventpair::create() = %s\n", zx_status_get_string(status));
    return status;
  }
  auto console = std::make_unique<Console>(loop.dispatcher(), std::move(event1), std::move(event2),
                                           std::move(rx_source), std::move(tx_sink));

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());
  if (zx::result status = outgoing.AddProtocol<fuchsia_hardware_pty::Device>(std::move(console));
      status.is_error()) {
    printf("console: outgoing.AddProtocol() = %s\n", status.status_string());
    return status.status_value();
  }

  if (zx::result status = outgoing.ServeFromStartupInfo(); status.is_error()) {
    printf("console: outgoing.ServeFromStartupInfo() = %s\n", status.status_string());
    return status.status_value();
  }

  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    printf("console: lop.Run() = %s\n", zx_status_get_string(status));
    return status;
  }
  return 0;
}
