// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/structured_backend/cpp/log_connection.h>

namespace fuchsia_logging::internal {

zx::result<LogConnection> LogConnection::Create(
    fidl::UnownedClientEnd<fuchsia_logger::LogSink> client_end) {
  zx::socket local, remote;
  if (zx_status_t status = zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote);
      status != ZX_OK) {
    return zx::error(status);
  }
  auto result = fidl::WireCall(client_end)->ConnectStructured(std::move(remote));

  if (!result.ok()) {
    return zx::error(result.error().status());
  }

  return zx::ok(LogConnection(std::move(local), {}));
}

zx::result<> LogConnection::FlushBuffer(fuchsia_syslog::LogBuffer& buffer) const {
  cpp20::span<const uint8_t> data = buffer.EndRecord();
  zx_status_t status;
  while (true) {
    status = socket_.write(0, data.data(), data.size_bytes(), nullptr);
    if (status == ZX_OK) {
      return zx::ok();
    }
    if (status != ZX_ERR_SHOULD_WAIT || !block_if_full_) {
      return zx::error(status);
    }
    zx_signals_t observed;
    status = socket_.wait_one(ZX_SOCKET_WRITABLE | ZX_SOCKET_PEER_CLOSED, zx::time::infinite(),
                              &observed);
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }
}

}  // namespace fuchsia_logging::internal
