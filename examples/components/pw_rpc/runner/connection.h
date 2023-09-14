// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_COMPONENTS_PW_RPC_RUNNER_CONNECTION_H_
#define EXAMPLES_COMPONENTS_PW_RPC_RUNNER_CONNECTION_H_

#include <array>
#include <cstdlib>
#include <map>
#include <mutex>
#include <vector>

#include "pw_bytes/span.h"
#include "pw_hdlc/decoder.h"
#include "pw_rpc/channel.h"
#include "pw_status/status.h"
#include "pw_stream/socket_stream.h"
#include "src/lib/fxl/synchronization/thread_annotations.h"

// Upper bound on the number of bytes to read from the stream at a time.
constexpr size_t kReadBufferSize = 1;
constexpr size_t kMaxTransmissionUnit = 1055;
constexpr size_t kDecoderBufferSize =
    pw::hdlc::Decoder::RequiredBufferSizeForFrameSize(kMaxTransmissionUnit);

// Represents a socket connection with an RPC channel Output.
class HdlcChannelSocketConnection {
 public:
  HdlcChannelSocketConnection() = default;
  explicit HdlcChannelSocketConnection(pw::stream::SocketStream stream)
      : stream_(std::move(stream)) {}
  HdlcChannelSocketConnection(HdlcChannelSocketConnection&&) = default;
  HdlcChannelSocketConnection& operator=(HdlcChannelSocketConnection&&) = default;
  ~HdlcChannelSocketConnection() { Close(); }

  // Reads any available data from the socket into the buffer.
  pw::Status ReadIntoBuffer();

  // Retrieves the next RPC packet from the buffer. The returned span will
  // remain valid until the next call to GetNextRpcPacket().
  //
  // If the buffer does not contain a complete packet, returns
  // Status::ResourceExhausted.
  pw::Result<std::pair<pw::ConstByteSpan, uint32_t>> GetNextRpcPacket();

  // Writes the given data into the socket.
  pw::Status Write(pw::ConstByteSpan data);

  // Closes the connection.
  void Close();

  int connection_fd() { return stream_.connection_fd(); }
  constexpr std::optional<uint64_t> address() const { return address_; }

 private:
  pw::stream::SocketStream stream_;
  std::vector<std::byte> input_buffer_;
  unsigned short pos_{0};
  bool returned_packet_{false};
  std::array<std::byte, kDecoderBufferSize> decoder_buffer_;
  pw::hdlc::Decoder decoder_{decoder_buffer_};
  std::optional<uint64_t> address_;
  std::array<std::byte, kReadBufferSize> data_;
};

struct ConnectionGroup {
  explicit ConnectionGroup(pw::stream::SocketStream stream);

  std::mutex mtx;
  // The actual connection to the offloaded program.
  HdlcChannelSocketConnection real_connection FXL_GUARDED_BY(mtx);
  // Multiplexed connections. Each connection may or may not have an address assigned yet.
  std::vector<HdlcChannelSocketConnection> virtual_connections FXL_GUARDED_BY(mtx);

  // Returns the (real or virtual) connection with this fd, if it exists.
  HdlcChannelSocketConnection* LookupByFd(int fd) FXL_EXCLUSIVE_LOCKS_REQUIRED(mtx);
  // Returns the (real or virtual) connection bound to this HDLC address, if it exists.
  HdlcChannelSocketConnection* LookupVirtualByAddress(uint64_t address)
      FXL_EXCLUSIVE_LOCKS_REQUIRED(mtx);
  // Closes the (real or virtual) connection with this fd.
  void CloseConnection(int fd) FXL_EXCLUSIVE_LOCKS_REQUIRED(mtx);
  // Returns all the fds associated with a connection.
  std::vector<int> GetAllFds() FXL_EXCLUSIVE_LOCKS_REQUIRED(mtx);
  // Returns true if the real connection has disconnected.
  bool RealDisconnected() FXL_EXCLUSIVE_LOCKS_REQUIRED(mtx);
};

#endif  // EXAMPLES_COMPONENTS_PW_RPC_RUNNER_CONNECTION_H_
