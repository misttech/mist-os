// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/validate/logs/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include "lib/syslog/cpp/log_level.h"
#include "lib/syslog/cpp/log_settings.h"
#include "lib/syslog/cpp/logging_backend.h"

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

class Puppet : public fuchsia::validate::logs::LogSinkPuppet {
 public:
  explicit Puppet(std::unique_ptr<sys::ComponentContext> context) : context_(std::move(context)) {
    context_->outgoing()->AddPublicService(sink_bindings_.GetHandler(this));
    syslog_backend::SetInterestChangedListener(
        +[](void* context, fuchsia_logging::LogSeverity severity) {
          syslog_backend::LogBuffer buffer;
          syslog_backend::BeginRecord(&buffer, severity, __FILE__, __LINE__, "Changed severity",
                                      nullptr);
          syslog_backend::FlushRecord(&buffer);
        },
        nullptr);
  }

  void StopInterestListener(StopInterestListenerCallback callback) override {
    fuchsia_logging::LogSettings settings;
    settings.disable_interest_listener = true;
    settings.min_log_level = fuchsia_logging::LOG_TRACE;
    syslog_backend::SetLogSettings(settings);
    callback();
  }

  void GetInfo(GetInfoCallback callback) override {
    fuchsia::validate::logs::PuppetInfo info;
    info.pid = GetKoid(zx_process_self());
    info.tid = GetKoid(zx_thread_self());
    callback(info);
  }

  void EmitLog(fuchsia::validate::logs::RecordSpec spec, EmitLogCallback callback) override {
    syslog_backend::LogBuffer buffer;
    fuchsia_logging::LogSeverity severity;
    switch (spec.record.severity) {
      case fuchsia::diagnostics::Severity::DEBUG:
        severity = fuchsia_logging::LOG_DEBUG;
        break;
      case fuchsia::diagnostics::Severity::ERROR:
        severity = fuchsia_logging::LOG_ERROR;
        break;
      case fuchsia::diagnostics::Severity::FATAL:
        severity = fuchsia_logging::LOG_FATAL;
        break;
      case fuchsia::diagnostics::Severity::INFO:
        severity = fuchsia_logging::LOG_INFO;
        break;
      case fuchsia::diagnostics::Severity::TRACE:
        severity = fuchsia_logging::LOG_TRACE;
        break;
      case fuchsia::diagnostics::Severity::WARN:
        severity = fuchsia_logging::LOG_WARNING;
        break;
    }
    syslog_backend::BeginRecord(&buffer, severity, spec.file.data(), spec.line, nullptr, nullptr);
    for (auto& arg : spec.record.arguments) {
      switch (arg.value.Which()) {
        case fuchsia::diagnostics::stream::Value::kUnknown:
        case fuchsia::diagnostics::stream::Value::Invalid:
          break;
        case fuchsia::diagnostics::stream::Value::kFloating:
          syslog_backend::WriteKeyValue(&buffer, arg.name.data(), arg.value.floating());
          break;
        case fuchsia::diagnostics::stream::Value::kSignedInt:
          syslog_backend::WriteKeyValue(&buffer, arg.name.data(), arg.value.signed_int());
          break;
        case fuchsia::diagnostics::stream::Value::kUnsignedInt:
          syslog_backend::WriteKeyValue(&buffer, arg.name.data(), arg.value.unsigned_int());
          break;
        case fuchsia::diagnostics::stream::Value::kText:
          syslog_backend::WriteKeyValue(&buffer, arg.name.data(), arg.value.text().data());
          break;
        case fuchsia::diagnostics::stream::Value::kBoolean:
          syslog_backend::WriteKeyValue(&buffer, arg.name.data(), arg.value.boolean());
          break;
      }
    }
    syslog_backend::FlushRecord(&buffer);
    callback();
  }

 private:
  std::unique_ptr<sys::ComponentContext> context_;
  fidl::BindingSet<fuchsia::validate::logs::LogSinkPuppet> sink_bindings_;
};

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  Puppet puppet(sys::ComponentContext::CreateAndServeOutgoingDirectory());
  // Note: This puppet is ran by a runner that
  // uses --test-invalid-unicode, which isn't directly passed to this puppet
  // but tells the Rust puppet runner that we are sending invalid UTF-8.
  FX_LOGS(INFO) << "Puppet started.\xc3\x28";
  loop.Run();
}
