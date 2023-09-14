// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/components/pw_rpc/runner/remote_endpoint.h"

#include <fcntl.h>
#include <fidl/fidl.examples.pigweed/cpp/fidl.h>
#include <lib/fdio/fd.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <lib/zx/socket.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>
#include <mutex>

#include "examples/components/pw_rpc/runner/connection.h"
#include "pw_stream/socket_stream.h"

void RemoteEndpoint::Connect(ConnectCompleter::Sync& completer) {
  std::shared_ptr<ConnectionGroup> connections = connections_.lock();
  if (connections == nullptr) {
    completer.Reply(fit::error(static_cast<int32_t>(ZX_ERR_NOT_FOUND)));
    return;
  }
  auto result = OpenConnection(connections);
  if (result.is_error()) {
    completer.Reply(fit::error(static_cast<int32_t>(result.error_value())));
    return;
  }
  zx::socket& conn = *result;
  completer.Reply(fit::success(std::move(conn)));
}

zx::result<zx::socket> OpenConnection(std::shared_ptr<ConnectionGroup> connections) {
  zx::socket c0, out;
  if (auto status = zx::socket::create(ZX_SOCKET_STREAM, &c0, &out); status != ZX_OK) {
    return zx::error(status);
  }

  int connection_fd;
  if (auto status = fdio_fd_create(c0.release(), &connection_fd); status != ZX_OK) {
    return zx::error(status);
  }
  const int flags = fcntl(connection_fd, F_GETFD, 0) | O_NONBLOCK;
  FX_CHECK(fcntl(connection_fd, F_SETFD, flags) == 0);

  pw::stream::SocketStream stream(connection_fd);
  std::lock_guard guard(connections->mtx);
  connections->virtual_connections.emplace_back(std::move(stream));

  return zx::success(std::move(out));
}

void RemoteEndpoint::handle_unknown_method(
    fidl::UnknownMethodMetadata<fidl_examples_pigweed::RemoteEndpoint> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_LOGS(WARNING) << "Received an unknown method with ordinal " << metadata.method_ordinal;
}
