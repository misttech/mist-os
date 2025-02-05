// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/logger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/async/wait.h>
#include <lib/diagnostics/reader/cpp/logs.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include <rapidjson/document.h>
#include <src/diagnostics/lib/cpp-log-decoder/log_decoder.h>
#include <src/lib/diagnostics/accessor2logger/log_message.h>
#include <src/lib/fsl/vmo/sized_vmo.h>
#include <src/lib/fsl/vmo/strings.h>
#include <src/lib/uuid/uuid.h>

constexpr size_t kMaxDatagramSize = 65536;

namespace log_tester {
class FakeLogSink : public fuchsia::logger::LogSink {
 public:
  explicit FakeLogSink(async_dispatcher_t* dispatcher, zx::channel channel)
      : dispatcher_(dispatcher) {
    fidl::InterfaceRequest<fuchsia::logger::LogSink> request(std::move(channel));
    bindings_.AddBinding(this, std::move(request), dispatcher);
  }

  /// Send this socket to be drained.
  ///
  /// See //zircon/system/ulib/syslog/include/lib/syslog/wire_format.h for what
  /// is expected to be received over the socket.
  void Connect(::zx::socket socket) override {
    // Not supported by this test.
    abort();
  }

  void WaitForInterestChange(WaitForInterestChangeCallback callback) override {
    // Ignored.
  }

  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override { abort(); }

  struct Wait : async_wait_t {
    FakeLogSink* this_ptr;
    Wait* next = this;
    Wait* prev = this;
  };

  static fuchsia::diagnostics::FormattedContent BytesToVmo(const uint8_t* bytes, size_t len) {
    std::string msg = DecodeMessageToString(bytes, len);
    fsl::SizedVmo vmo;
    fsl::VmoFromString(msg, &vmo);
    fuchsia::diagnostics::FormattedContent content;
    fuchsia::mem::Buffer buffer;
    buffer.vmo = std::move(vmo.vmo());
    buffer.size = msg.size();
    content.set_json(std::move(buffer));
    return content;
  }

  static std::string DecodeMessageToString(const uint8_t* data, size_t len) {
    auto raw_message = fuchsia_decode_log_message_to_json(data, len);
    std::string ret = raw_message;
    fuchsia_free_decoded_log_message(raw_message);
    return ret;
  }

  void OnPeerClosed() { callback_.value()(std::nullopt, ZX_ERR_PEER_CLOSED); }

  void OnDataAvailable(zx_handle_t socket) {
    std::unique_ptr<unsigned char[]> data = std::make_unique<unsigned char[]>(kMaxDatagramSize);
    size_t actual = 0;
    zx_status_t status = zx_socket_read(socket, 0, data.get(), kMaxDatagramSize, &actual);
    if (status != ZX_OK) {
      callback_.value()(std::nullopt, status);
      return;
    }
    auto content = BytesToVmo(data.get(), actual);
    callback_.value()(std::make_optional(std::move(content)), ZX_OK);
  }

  static void OnDataAvailable_C(async_dispatcher_t* dispatcher, async_wait_t* raw,
                                zx_status_t status, const zx_packet_signal_t* signal) {
    switch (status) {
      case ZX_OK:
        static_cast<Wait*>(raw)->this_ptr->OnDataAvailable(raw->object);
        async_begin_wait(dispatcher, raw);
        break;
      case ZX_ERR_PEER_CLOSED:
        zx_handle_close(raw->object);
        static_cast<Wait*>(raw)->this_ptr->OnPeerClosed();
        break;
    }
  }

  /// Send this socket to be drained, using the structured logs format.
  ///
  /// See https://fuchsia.dev/fuchsia-src/reference/platform-spec/diagnostics/logs-encoding?hl=en
  /// for what is expected to be received over the socket.
  void ConnectStructured(::zx::socket socket) override {
    Wait* wait = new Wait();
    waits_.push_back(wait);
    wait->this_ptr = this;
    wait->object = socket.release();
    wait->handler = OnDataAvailable_C;
    wait->options = 0;
    wait->trigger = ZX_SOCKET_PEER_CLOSED | ZX_SOCKET_READABLE;
    async_begin_wait(dispatcher_, wait);
  }

  void Collect(std::function<void(std::optional<fuchsia::diagnostics::FormattedContent> content,
                                  zx_status_t status)>
                   callback) {
    callback_ = std::move(callback);
  }

  ~FakeLogSink() override {
    for (auto& wait : waits_) {
      async_cancel_wait(dispatcher_, wait);
      delete wait;
    }
  }

 private:
  std::vector<Wait*> waits_;
  fidl::BindingSet<fuchsia::logger::LogSink> bindings_;
  std::optional<std::function<void(std::optional<fuchsia::diagnostics::FormattedContent> content,
                                   zx_status_t status)>>
      callback_;
  async_dispatcher_t* dispatcher_;
};

void ParseFormattedContent(fuchsia::diagnostics::FormattedContent content,
                           std::vector<fuchsia::logger::LogMessage>& output) {
  auto chunk_result =
      diagnostics::accessor2logger::ConvertFormattedContentToLogMessages(std::move(content));
  auto messages = chunk_result.take_value();  // throws exception if conversion fails.
  for (auto& msg : messages) {
    std::string formatted = msg.value().msg;
    output.push_back(msg.take_value());
  }
}

std::vector<fuchsia::logger::LogMessage> RetrieveLogsAsLogMessage(zx::channel remote) {
  // Close channel (reset to default Archivist)
  fuchsia_logging::LogSettingsBuilder builder;
  builder.BuildAndInitialize();
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  std::vector<fuchsia::logger::LogMessage> ret;
  auto log_service = std::make_unique<FakeLogSink>(loop.dispatcher(), std::move(remote));
  log_service->Collect(
      [&](std::optional<fuchsia::diagnostics::FormattedContent> content, zx_status_t status) {
        if (status != ZX_OK) {
          loop.Quit();
          return;
        }
        assert(content.has_value());
        ParseFormattedContent(std::move(*content), ret);
      });
  loop.Run();
  return ret;
}

std::string RetrieveLogs(zx::channel remote) {
  std::stringstream stream;
  for (const auto& value : RetrieveLogsAsLogMessage(std::move(remote))) {
    stream << value.msg << std::endl;
  }
  return stream.str();
}

/// Converts logs in the structured socket to LogMessages in feedback format.
std::vector<diagnostics::reader::LogsData> RetrieveLogsAsLogMessage(const zx::socket& remote) {
  std::unique_ptr<unsigned char[]> data = std::make_unique<unsigned char[]>(kMaxDatagramSize);
  size_t actual = 0;
  remote.read(0, data.get(), kMaxDatagramSize, &actual);
  rapidjson::Document d;
  d.Parse(FakeLogSink::DecodeMessageToString(data.get(), actual));
  std::vector<diagnostics::reader::LogsData> ret;
  auto logs = d.GetArray();
  ret.reserve(logs.Size());
  for (auto& log : logs) {
    rapidjson::Document log_document;
    log_document.CopyFrom(log, d.GetAllocator());
    ret.emplace_back(std::move(log_document));
  }
  return ret;
}

zx::channel SetupFakeLog(bool wait_for_initial_interest, FuchsiaLogSeverity severity) {
  zx::channel channels[2];
  zx::channel::create(0, &channels[0], &channels[1]);
  fuchsia_logging::LogSettingsBuilder builder;
  builder.DisableWaitForInitialInterest()
      .WithMinLogSeverity(severity)
      .WithLogSink(channels[0].release())
      .BuildAndInitialize();
  return std::move(channels[1]);
}
}  // namespace log_tester
