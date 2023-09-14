// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/components/pw_rpc/runner/multiplexer.h"

#include <lib/syslog/cpp/macros.h>
#include <poll.h>

#include <mutex>
#include <thread>

#include "pw_status/status.h"

namespace {

constexpr int kPollTimeoutMs = 10000;

pw::Status Poll(std::vector<pollfd>& poll_fds, int timeout_ms) {
  const int num_ready = poll(poll_fds.data(), poll_fds.size(), timeout_ms);
  if (num_ready < 0) {
    return pw::Status::Unknown();
  }
  return pw::OkStatus();
}

}  // namespace

void Multiplexer::Detach() {
  Multiplexer self{std::move(*this)};
  std::thread thread([self = std::move(self)]() mutable { self.Run(); });
  thread.detach();
}

void Multiplexer::Run() {
  while (true) {
    std::vector<int> ready_streams = PollForData();
    if (ready_streams.empty()) {
      break;
    }
    if (!ReadData(ready_streams)) {
      break;
    }
    if (!ForwardPackets()) {
      break;
    }
  }
  // Closing this end will signal the runner to exit the component.
  disconnect_event_.reset();
}

std::vector<int> Multiplexer::PollForData() {
  while (true) {
    std::shared_ptr<ConnectionGroup> connections = connections_.lock();
    if (connections == nullptr) {
      return {};
    }
    std::vector<pollfd> poll_streams;
    {
      std::lock_guard lock(connections->mtx);
      if (connections->RealDisconnected()) {
        return {};
      }
      for (int fd : connections->GetAllFds()) {
        poll_streams.push_back(pollfd{.fd = fd, .events = POLLIN, .revents = 0});
      }
      connections = nullptr;
    }
    if (auto res = Poll(poll_streams, kPollTimeoutMs); !res.ok()) {
      FX_SLOG(FATAL, "Failed to poll connections", KV("status", res.str()));
    }
    connections = connections_.lock();
    if (connections == nullptr) {
      return {};
    }
    std::lock_guard lock(connections->mtx);
    std::vector<int> ready_streams;
    for (const pollfd& poll_fd : poll_streams) {
      if (poll_fd.revents == 0) {
        // not ready yet.
      } else if (poll_fd.revents & POLLIN) {
        ready_streams.push_back(poll_fd.fd);
      } else {
        // Hangup, close connection.
        connections->CloseConnection(poll_fd.fd);
        if (connections->RealDisconnected()) {
          return {};
        }
      }
    }
    if (!ready_streams.empty()) {
      return ready_streams;
    }
  }
}

bool Multiplexer::ReadData(const std::vector<int>& streams) {
  std::shared_ptr<ConnectionGroup> connections = connections_.lock();
  if (connections == nullptr) {
    return false;
  }

  std::lock_guard lock(connections->mtx);
  for (int stream : streams) {
    HdlcChannelSocketConnection* connection = connections->LookupByFd(stream);
    if (connection == nullptr) {
      continue;
    }
    const pw::Status status = connection->ReadIntoBuffer();
    if (status.IsOutOfRange()) {
      // Peer disconnected.
      connections->CloseConnection(stream);
      if (connections->RealDisconnected()) {
        return false;
      }
    } else if (!status.ok()) {
      FX_SLOG(ERROR, "Read failed", KV("status", status.str()));
    }
  }
  return true;
}

bool Multiplexer::ForwardPackets() {
  std::shared_ptr<ConnectionGroup> connections = connections_.lock();
  if (connections == nullptr) {
    return false;
  }

  std::lock_guard lock(connections->mtx);
  bool try_again = true;
  while (try_again) {
    try_again = false;
    {
      auto res = connections->real_connection.GetNextRpcPacket();
      if (res.ok()) {
        try_again = true;
        pw::ConstByteSpan& bytes = res->first;
        const uint64_t address = res->second;
        HdlcChannelSocketConnection* out_connection = connections->LookupVirtualByAddress(address);
        if (out_connection != nullptr) {
          const pw::Status status = out_connection->Write(bytes);
          if (status.IsOutOfRange()) {
            // Peer disconnected.
            connections->CloseConnection(out_connection->connection_fd());
          } else if (!status.ok()) {
            FX_SLOG(ERROR, "Failed to send inbound packet", KV("address", address),
                    KV("status", status.str()));
          }
        }
      } else if (res.status().IsResourceExhausted()) {
        // No complete packet yet.
      } else {
        FX_SLOG(ERROR, "Failed to read inbound packet", KV("status", res.status().str()));
      }
    }
    for (HdlcChannelSocketConnection& connection : connections->virtual_connections) {
      auto res = connection.GetNextRpcPacket();
      if (res.ok()) {
        try_again = true;
        pw::ConstByteSpan& bytes = res->first;
        const uint64_t address = res->second;
        auto status = connections->real_connection.Write(bytes);
        if (status.IsOutOfRange()) {
          // Peer disconnected.
          return false;
        } else if (!status.ok()) {
          FX_SLOG(ERROR, "Failed to send outbound packet", KV("address", address),
                  KV("status", status.str()));
        }
      } else if (res.status().IsResourceExhausted()) {
        // No complete packet yet.
      } else {
        FX_SLOG(ERROR, "Failed to read outbound packet", KV("status", res.status().str()));
      }
    }
  }

  return true;
}
