// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/components/pw_rpc/runner/log_proxy.h"

#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/macros.h>

#include <optional>
#include <thread>

#include "pw_assert/assert.h"
#include "pw_assert/check.h"
#include "pw_hdlc/rpc_channel.h"
#include "pw_hdlc/rpc_packets.h"
#include "pw_log/levels.h"
#include "pw_log/proto/log.pwpb.h"
#include "pw_log/proto/log.raw_rpc.pb.h"
#include "pw_protobuf/decoder.h"
#include "pw_rpc/channel.h"
#include "pw_rpc/client.h"
#include "pw_span/span.h"
#include "pw_stream/socket_stream.h"

namespace {

constexpr uint64_t kLoggingRpcAddress = 10000;

template <typename S, uint32_t kChannelId = 10000, size_t kMTU = 1055,
          uint64_t kRpcAddress = kLoggingRpcAddress>
class SocketClient {
 public:
  explicit SocketClient(pw::stream::SocketStream stream) : stream_(std::move(stream)) {}

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

pw::Status DecodeOptionallyTokenizedData(pw::protobuf::Decoder& entry_decoder, std::string* out) {
  pw::ConstByteSpan tokenized_data;
  PW_TRY(entry_decoder.ReadBytes(&tokenized_data));
  *out = std::string(std::string_view(reinterpret_cast<const char*>(tokenized_data.data()),
                                      tokenized_data.size()));
  return pw::OkStatus();
}

#define PW_TRY_NEXT(expr) _PW_TRY_NEXT(_PW_TRY_UNIQUE(__LINE__), expr)

#define _PW_TRY_NEXT(result, expr)                                      \
  do {                                                                  \
    if (auto result = (expr); !result.ok() && !result.IsOutOfRange()) { \
      return ::pw::internal::ConvertToStatus(result);                   \
    }                                                                   \
  } while (0)

fuchsia_logging::LogSeverity ConvertLogLevel(uint8_t level) {
  switch (level) {
    case PW_LOG_LEVEL_DEBUG:
      return fuchsia_logging::LOG_DEBUG;
    case PW_LOG_LEVEL_INFO:
      return fuchsia_logging::LOG_INFO;
    case PW_LOG_LEVEL_WARN:
      return fuchsia_logging::LOG_WARNING;
    case PW_LOG_LEVEL_ERROR:
    case PW_LOG_LEVEL_CRITICAL:
      return fuchsia_logging::LOG_ERROR;
    case PW_LOG_LEVEL_FATAL:
      return fuchsia_logging::LOG_FATAL;
    default:
      return fuchsia_logging::LOG_INFO;
  }
}

pw::Status LogEntry(pw::protobuf::Decoder& entry_decoder, const zx::socket& log_socket) {
  PW_TRY_NEXT(entry_decoder.Next());
  if (entry_decoder.FieldNumber() !=
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kMessage)) {
    // Valid log, but no message
    return pw::OkStatus();
  }
  std::string message;
  PW_TRY(DecodeOptionallyTokenizedData(entry_decoder, &message));
  if (message.empty()) {
    // Valid log, but no message
    return pw::OkStatus();
  }
  PW_TRY_NEXT(entry_decoder.Next());

  uint32_t line = 0;
  fuchsia_logging::LogSeverity level = fuchsia_logging::LOG_INFO;
  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kLineLevel)) {
    uint32_t line_level;
    PW_TRY(entry_decoder.ReadUint32(&line_level));
    // Fuchsia and pigweed log severity codes are compatible
    level = ConvertLogLevel(static_cast<uint8_t>(line_level & PW_LOG_LEVEL_BITMASK));
    line = (line_level & ~PW_LOG_LEVEL_BITMASK) >> PW_LOG_LEVEL_BITS;
    PW_TRY_NEXT(entry_decoder.Next());
  }

  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kFlags)) {
    PW_TRY_NEXT(entry_decoder.Next());
  }

  // TODO: forward this?
  if (entry_decoder.FieldNumber() ==
          static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kTimestamp) ||
      entry_decoder.FieldNumber() ==
          static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kTimeSinceLastEntry

                                )) {
    PW_TRY_NEXT(entry_decoder.Next());
  }

  // TODO: forward this?
  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kDropped)) {
    PW_TRY_NEXT(entry_decoder.Next());
  }

  // TODO: forward this?
  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kModule)) {
    PW_TRY_NEXT(entry_decoder.Next());
  }

  std::string file;
  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kFile)) {
    PW_TRY(DecodeOptionallyTokenizedData(entry_decoder, &file));
    PW_TRY_NEXT(entry_decoder.Next());
  }

  if (entry_decoder.FieldNumber() ==
      static_cast<uint32_t>(pw::log::pwpb::LogEntry::Fields::kThread)) {
    PW_TRY_NEXT(entry_decoder.Next());
  }

  if (log_socket.is_valid()) {
    syslog_backend::LogBuffer log_buffer;
    syslog_backend::BeginRecordWithSocket(&log_buffer, level, file.c_str(), line, message.c_str(),
                                          /* condition */ nullptr, log_socket.get());
    syslog_backend::FlushRecord(&log_buffer);
  }
  return pw::OkStatus();
}

void ListenNext(SocketClient<pw::log::pw_rpc::raw::Logs>& sc, const zx::socket& log_socket,
                pw::ConstByteSpan response) {
  pw::protobuf::Decoder entries_decoder(response);
  while (entries_decoder.Next().ok()) {
    if (static_cast<pw::log::pwpb::LogEntries::Fields>(entries_decoder.FieldNumber()) ==
        pw::log::pwpb::LogEntries::Fields::kEntries) {
      pw::ConstByteSpan entry;
      if (!entries_decoder.ReadBytes(&entry).ok()) {
        FX_SLOG(WARNING, "Failed to decode pigweed log entry");
        continue;
      }
      pw::protobuf::Decoder entry_decoder(entry);
      ::pw::Status status = LogEntry(entry_decoder, log_socket);
      if (!status.ok()) {
        FX_SLOG(WARNING, "Encountered invalid pigweed log entry");
      }
    } else if (static_cast<pw::log::pwpb::LogEntries::Fields>(entries_decoder.FieldNumber()) ==
               pw::log::pwpb::LogEntries::Fields::kFirstEntrySequenceId) {
      continue;
    }
  }
}

}  // namespace

void LogProxy::Detach() {
  LogProxy self{std::move(*this)};
  std::thread thread([self = std::move(self)]() mutable { self.Run(); });
  thread.detach();
}

void LogProxy::Run() {
  SocketClient<pw::log::pw_rpc::raw::Logs> sc{std::move(stream_)};
  auto call = sc->Listen(
      {}, [&sc, this](pw::ConstByteSpan response) { ListenNext(sc, log_socket_, response); },
      [&](pw::Status status) {
        if (!status.ok()) {
          FX_SLOG(INFO, "Log listener completed with error", KV("status", status.str()));
        }
        sc.terminate();
      },
      [&](pw::Status status) {
        FX_SLOG(INFO, "Failed to read logs", KV("status", status.str()));
        sc.terminate();
      });

  sc.ProcessPackets();

  syslog_backend::LogBuffer log_buffer;
  syslog_backend::BeginRecordWithSocket(&log_buffer, fuchsia_logging::LOG_INFO, __FILE__, __LINE__,
                                        "Connection to proxy has terminated. Exiting.",
                                        /* condition */ nullptr, log_socket_.get());
  syslog_backend::FlushRecord(&log_buffer);
}
