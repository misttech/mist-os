// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include <cstdlib>
#include <iostream>

#include "pw_hdlc/rpc_channel.h"
#include "pw_hdlc/rpc_packets.h"
#include "pw_rpc/channel.h"
#include "pw_rpc/client.h"
#include "pw_rpc/echo.rpc.pwpb.h"
#include "pw_span/span.h"
#include "pw_stream/socket_stream.h"

const char* kEchoHost = "your-ip-address";
constexpr uint16_t kEchoPort = 33000;

template <typename S, uint32_t kChannelId = 1, size_t kMTU = 1055,
          uint8_t kRpcAddress = pw::hdlc::kDefaultRpcAddress>
class SocketClient {
 public:
  SocketClient() = default;

  void Connect(const char* host, uint16_t port) { stream_.Connect(host, port); }

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
        should_terminate_.test_and_set();
      }

      if (should_terminate_.test()) {
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
    should_terminate_.test_and_set();
    stream_.Close();
  }

 private:
  pw::stream::SocketStream stream_;
  pw::hdlc::FixedMtuChannelOutput<kMTU> channel_output_{stream_, kRpcAddress, "socket"};
  pw::rpc::Channel channel_{pw::rpc::Channel::Create<kChannelId>(&channel_output_)};
  pw::rpc::Client client_{pw::span(&channel_, 1)};
  typename S::Client service_client_{client_, kChannelId};
  std::atomic_flag should_terminate_ = ATOMIC_FLAG_INIT;
};

int main(int argc, const char* argv[], char* envp[]) {
  fuchsia_logging::SetTags({"pw_rpc_echo"});

  // Print a greeting to syslog
  FX_SLOG(INFO, "Starting up.");

  SocketClient<pw::rpc::pw_rpc::pwpb::EchoService> sc{};
  sc.Connect(kEchoHost, kEchoPort);

  auto call = sc->Echo(
      {.msg = "Hello, Pigweed"},
      [&](const pw::rpc::pwpb::EchoMessage::Message& message, pw::Status status) {
        FX_SLOG(INFO, "Received echo reply", KV("msg", message.msg.c_str()));
        sc.terminate();
      },
      [&](pw::Status status) {
        FX_SLOG(INFO, "Received error", KV("status", status.str()));
        sc.terminate();
      });

  FX_SLOG(INFO, "Sent echo request.");

  sc.ProcessPackets();

  FX_SLOG(INFO, "Done.");
  return 0;
}
// [END main]
