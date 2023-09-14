// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_COMPONENTS_PW_RPC_RUNNER_MULTIPLEXER_H_
#define EXAMPLES_COMPONENTS_PW_RPC_RUNNER_MULTIPLEXER_H_

#include <lib/zx/eventpair.h>

#include <memory>
#include <vector>

#include "examples/components/pw_rpc/runner/connection.h"

class Multiplexer {
 public:
  Multiplexer() = default;
  Multiplexer(Multiplexer&&) = default;
  Multiplexer& operator=(Multiplexer&&) = default;
  ~Multiplexer() = default;

  // Instantiates a Multiplexer that will multiplex the given ConnectionGroup. When the remote
  // RPC peer disconnects, closes `disconnect_event`.
  Multiplexer(std::weak_ptr<ConnectionGroup> connections, zx::eventpair disconnect_event)
      : connections_(std::move(connections)), disconnect_event_(std::move(disconnect_event)) {}

  void Detach();

 private:
  void Run();
  // Returns fds for all the (real or virtual) connections that have data ready. Blocks until
  // at least one connection is ready.
  //
  // If the real connection has disconnected, returns an empty vector.
  std::vector<int> PollForData();
  // Read data from the given streams into the buffer.
  //
  // If the real connection has disconnected, returns an empty vector.
  bool ReadData(const std::vector<int>& streams);
  // Forwards packets from the real to virtual connections and vice versa.
  //
  // If the real connection has disconnected, returns false.
  bool ForwardPackets();

  std::weak_ptr<ConnectionGroup> connections_;
  zx::eventpair disconnect_event_;
};

#endif  // EXAMPLES_COMPONENTS_PW_RPC_RUNNER_MULTIPLEXER_H_
