// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/logger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/function.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <zxtest/zxtest.h>

#include "lib/syslog/cpp/logging_backend.h"

class LogTest : public zxtest::Test {};

namespace {

class LogListener : public fuchsia::logger::LogListenerSafe {
 public:
  explicit LogListener(fit::function<void(::fuchsia::logger::LogMessage)> message_callback,
                       zx::channel channel)
      : binding_(this), message_callback_(std::move(message_callback)) {
    binding_.Bind(std::move(channel));
  }

  /// Called for single messages.
  ///
  /// The return value is used for flow control, and implementers should acknowledge receipt of
  /// each message in order to continue receiving future messages.
  void Log(::fuchsia::logger::LogMessage log, LogCallback callback) override {
    message_callback_(std::move(log));
    callback();
  }

  /// Called when serving cached logs.
  ///
  /// Max logs size per call is `MAX_LOG_MANY_SIZE_BYTES` bytes.
  ///
  /// The return value is used for flow control, and implementers should acknowledge receipt of
  /// each batch in order to continue receiving future messages.
  void LogMany(::std::vector<::fuchsia::logger::LogMessage> log,
               LogManyCallback callback) override {
    for (auto& msg : log) {
      message_callback_(std::move(msg));
    }
    callback();
  }

  /// Called when this listener was passed to `DumpLogsSafe()` and all cached logs have been sent.
  void Done() override {}

 private:
  fidl::Binding<fuchsia::logger::LogListenerSafe> binding_;
  fit::function<void(::fuchsia::logger::LogMessage)> message_callback_;
};

TEST_F(LogTest, LegacyFormat) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FX_SLOG(INFO, "Test message", KV("tag", "test"), KV("tag", "test2"));
  fuchsia::logger::LogPtr accessor;
  auto context = sys::ComponentContext::Create();
  context->svc()->Connect(accessor.NewRequest());
  zx::channel local, remote;
  zx::channel::create(0, &local, &remote);
  fidl::InterfaceHandle<fuchsia::logger::LogListenerSafe> listener_interface(std::move(remote));
  accessor->ListenSafe(std::move(listener_interface),
                       std::unique_ptr<fuchsia::logger::LogFilterOptions>(nullptr));
  LogListener listener(
      [&](::fuchsia::logger::LogMessage message) {
        ASSERT_EQ(message.msg,
                  "[sdk/ctf/tests/fidl/fuchsia.diagnostics/logging_test.cc(61)] Test message");
        ASSERT_EQ(message.tags.size(), 2);
        ASSERT_EQ(message.tags[0], "test");
        ASSERT_EQ(message.tags[1], "test2");
        loop.Quit();
      },
      std::move(local));
  loop.Run();
}
}  // namespace
