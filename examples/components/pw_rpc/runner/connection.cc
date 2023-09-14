// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/components/pw_rpc/runner/connection.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <optional>

#include "pw_span/span.h"

// Reads any available data from the socket into the buffer.
pw::Status HdlcChannelSocketConnection::ReadIntoBuffer() {
  auto res = stream_.Read(data_);
  if (!res.ok()) {
    return res.status();
  }
  pw::ByteSpan& read_data = *res;
  input_buffer_.insert(input_buffer_.end(), read_data.begin(), read_data.end());
  return pw::OkStatus();
}

pw::Result<std::pair<pw::ConstByteSpan, uint32_t>> HdlcChannelSocketConnection::GetNextRpcPacket() {
  if (returned_packet_) {
    // A packet was returned last time, so reset the buffer.
    std::vector<std::byte> new_input_buffer;
    new_input_buffer.reserve(input_buffer_.size() - pos_);
    std::copy(input_buffer_.begin() + pos_, input_buffer_.end(), new_input_buffer.begin());
    input_buffer_.swap(new_input_buffer);
    pos_ = 0;
    returned_packet_ = false;
  }
  while (pos_ < input_buffer_.size()) {
    const std::byte byte = input_buffer_[pos_++];
    const auto res = decoder_.Process(byte);
    if (!res.ok()) {
      continue;
    }
    const pw::hdlc::Frame& frame = *res;
    // Bind the packet's address to this stream.
    if (!address_.has_value()) {
      address_.emplace(frame.address());
    }
    returned_packet_ = true;
    auto bytes = pw::span{&input_buffer_[0], &input_buffer_[pos_]};
    return std::make_pair(bytes, frame.address());
  }
  return pw::Status::ResourceExhausted();
}

pw::Status HdlcChannelSocketConnection::Write(pw::ConstByteSpan data) {
  return stream_.Write(data);
}

void HdlcChannelSocketConnection::Close() {
  stream_.Close();
  input_buffer_.clear();
  pos_ = 0;
  returned_packet_ = false;
  address_.reset();
}

ConnectionGroup::ConnectionGroup(pw::stream::SocketStream stream)
    : real_connection(std::move(stream)) {}

HdlcChannelSocketConnection* ConnectionGroup::LookupByFd(int fd) {
  if (real_connection.connection_fd() == fd) {
    return &real_connection;
  }
  for (HdlcChannelSocketConnection& c : virtual_connections) {
    if (c.connection_fd() == fd) {
      return &c;
    }
  }
  return nullptr;
}

HdlcChannelSocketConnection* ConnectionGroup::LookupVirtualByAddress(uint64_t address) {
  for (HdlcChannelSocketConnection& c : virtual_connections) {
    if (c.address().has_value() && *c.address() == address) {
      return &c;
    }
  }
  return nullptr;
}

void ConnectionGroup::CloseConnection(int fd) {
  if (real_connection.connection_fd() == fd) {
    real_connection.Close();
    return;
  }
  for (auto i = virtual_connections.begin(); i != virtual_connections.end(); ++i) {
    HdlcChannelSocketConnection& c = *i;
    if (c.connection_fd() == fd) {
      c.Close();
      virtual_connections.erase(i);
      return;
    }
  }
}

std::vector<int> ConnectionGroup::GetAllFds() {
  std::vector<int> res;
  res.reserve(virtual_connections.size() + 1);
  res.push_back(real_connection.connection_fd());
  for (HdlcChannelSocketConnection& c : virtual_connections) {
    res.push_back(c.connection_fd());
  }
  return res;
}

bool ConnectionGroup::RealDisconnected() { return real_connection.connection_fd() < 0; }
