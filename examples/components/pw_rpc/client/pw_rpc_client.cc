// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.examples.pigweed/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/fd.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/socket.h>
#include <sys/socket.h>
#include <unistd.h>
#include <zircon/status.h>

#include <cstdlib>

#include "pw_assert/assert.h"
#include "pw_bytes/span.h"
#include "pw_hdlc/rpc_channel.h"
#include "pw_hdlc/rpc_packets.h"
#include "pw_rpc/channel.h"
#include "pw_rpc/client.h"
#include "pw_rpc/echo.rpc.pwpb.h"
#include "pw_span/span.h"
#include "pw_stream/socket_stream.h"

namespace {

template <typename S, uint32_t kChannelId = 1, size_t kMTU = 1055,
          uint8_t kRpcAddress = pw::hdlc::kDefaultRpcAddress>
class SocketClient {
 public:
  explicit SocketClient(int connection_fd) : stream_(connection_fd) {}

  typename S::Client* operator->() { return &service_client_; }

  void ProcessPackets() {
    constexpr size_t kDecoderBufferSize = pw::hdlc::Decoder::RequiredBufferSizeForFrameSize(kMTU);
    std::array<std::byte, kDecoderBufferSize> decode_buffer;
    pw::hdlc::Decoder decoder(decode_buffer);

    while (true) {
      std::byte byte[1];
      pw::Result<pw::ByteSpan> read = stream_.Read(byte);

      if (read.status() == pw::Status::OutOfRange()) {
        // Channel closed.
        should_terminate_ = true;
      }

      if (should_terminate_) {
        return;
      }

      if (!read.ok() || read->empty()) {
        continue;
      }

      pw::Result<pw::hdlc::Frame> result = decoder.Process(*byte);
      if (result.ok()) {
        pw::hdlc::Frame& frame = result.value();
        if (frame.address() == kRpcAddress) {
          PW_ASSERT(client_.ProcessPacket(frame.data()).ok());
        }
      }
    }
  }

  void terminate() {
    should_terminate_ = true;
    stream_.Close();
  }

 private:
  pw::stream::SocketStream stream_;
  pw::hdlc::FixedMtuChannelOutput<kMTU> channel_output_{stream_, kRpcAddress, "socket"};
  pw::rpc::Channel channel_{pw::rpc::Channel::Create<kChannelId>(&channel_output_)};
  pw::rpc::Client client_{pw::span(&channel_, 1)};
  typename S::Client service_client_{client_, kChannelId};
  bool should_terminate_ = false;
};

void DoConnect(fidl::Result<fidl_examples_pigweed::RemoteEndpoint::Connect>& result) {
  FX_CHECK(result.is_ok()) << "Failed to connect to remote endpoint: " << result.error_value();
  int connection_fd;
  zx_status_t status = fdio_fd_create(result->connection().release(), &connection_fd);
  FX_CHECK(status == ZX_OK) << "Failed to bind file descriptor: " << zx_status_get_string(status);

  SocketClient<pw::rpc::pw_rpc::pwpb::EchoService> sc{connection_fd};
  FX_SLOG(INFO, "Sending echo request.");
  auto echo_call = sc->Echo(
      {.msg = "Hello, Pigweed"},
      [&](const pw::rpc::pwpb::EchoMessage::Message& message, pw::Status status) {
        FX_SLOG(INFO, "Received echo reply", KV("msg", message.msg.c_str()));
        sc.terminate();
      },
      [&](pw::Status status) {
        FX_SLOG(INFO, "Received error", KV("status", status.str()));
        sc.terminate();
      });

  FX_SLOG(INFO, "Processing packets.");
  sc.ProcessPackets();

  FX_SLOG(INFO, "Done.");
}

}  // namespace

int main(int argc, const char* argv[], char* envp[]) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  // Print a greeting to syslog
  FX_SLOG(INFO, "Starting up.");
  zx::result client_end = component::Connect<fidl_examples_pigweed::RemoteEndpoint>();
  FX_CHECK(client_end.is_ok()) << "Failed to connect to remote endpoint: "
                               << client_end.status_string();
  fidl::Client<fidl_examples_pigweed::RemoteEndpoint> remote_endpoint;
  remote_endpoint.Bind(std::move(*client_end), loop.dispatcher());
  remote_endpoint->Connect().ThenExactlyOnce(
      [&](fidl::Result<fidl_examples_pigweed::RemoteEndpoint::Connect>& result) {
        DoConnect(result);
        loop.Quit();
      });

  loop.Run();
  return 0;
}

// [END main]
