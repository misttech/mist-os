// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_COMPONENTS_PW_RPC_RUNNER_REMOTE_ENDPOINT_H_
#define EXAMPLES_COMPONENTS_PW_RPC_RUNNER_REMOTE_ENDPOINT_H_

#include <fidl/fidl.examples.pigweed/cpp/fidl.h>
#include <lib/zx/result.h>
#include <lib/zx/socket.h>

#include <memory>
#include <string>

#include "examples/components/pw_rpc/runner/connection.h"

class RemoteEndpoint : public fidl::Server<fidl_examples_pigweed::RemoteEndpoint> {
 public:
  explicit RemoteEndpoint(std::weak_ptr<ConnectionGroup> c) : connections_(std::move(c)) {}

  void Connect(ConnectCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fidl_examples_pigweed::RemoteEndpoint> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  std::weak_ptr<ConnectionGroup> connections_;
};

zx::result<zx::socket> OpenConnection(std::shared_ptr<ConnectionGroup> connections);

#endif  // EXAMPLES_COMPONENTS_PW_RPC_RUNNER_REMOTE_ENDPOINT_H_
