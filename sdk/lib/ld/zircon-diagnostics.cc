// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/result.h>
#include <unistd.h>
#include <zircon/syscalls/log.h>

#include <cassert>

#include "startup-diagnostics.h"
#include "stdio/printf_core/wrapper.h"
#include "zircon.h"

namespace ld {
namespace {

constexpr size_t kBufferSize = ZX_LOG_RECORD_DATA_MAX;

constexpr zx_signals_t kSocketWait = ZX_SOCKET_WRITABLE | ZX_SOCKET_PEER_CLOSED;

void DebuglogWrite(StartupData& startup, std::string_view str) {
  if (size_t chars = str.size() - (str.back() == '\n' ? 1 : 0); chars > 0) {
    startup.debuglog.write(0, str.data(), chars);
  }
}

zx::result<size_t> SocketWrite(StartupData& startup, std::string_view str) {
  zx_status_t status;
  do {
    size_t wrote = 0;
    status = startup.log_socket.write(0, str.data(), str.size(), &wrote);
    if (status == ZX_OK) {
      return zx::ok(wrote);
    }
    if (status == ZX_ERR_SHOULD_WAIT) {
      zx_signals_t pending = 0;
      status = startup.log_socket.wait_one(kSocketWait, zx::time::infinite(), &pending);
    }
  } while (status == ZX_OK);
  return zx::error(status);
}

int SocketWriteAll(StartupData& startup, std::string_view str) {
  int wrote = 0;
  while (!str.empty()) {
    auto result = SocketWrite(startup, str);
    if (result.is_error()) {
      return wrote == 0 ? -1 : wrote;
    }
    str.remove_prefix(result.value());
    wrote += static_cast<int>(result.value());
  }
  return wrote;
}

}  // namespace

// The formatted message from elfldltl::PrintfDiagnosticsReport should be a
// single line with no newline, but we tell Printf to add one (see below).
// If the whole line fits in the buffer, then this callback will only be made
// once.
int StartupData::Log(std::string_view str) {
  assert(!str.empty());

  // If we have a debuglog handle, use that.  It's packet-oriented with each
  // message expected to be a single line without newlines.  So remove the
  // newline that was added.  If the line was too long, it will look like two
  // separate log lines.
  if (debuglog) {
    DebuglogWrite(*this, str);
  }

  if (log_socket) {
    // We might instead (or also?) have a socket, where the messages are
    // easier to capture at the other end.  This is a stream-oriented socket
    // (aka an fdio pipe), where newlines are expected.
    return SocketWriteAll(*this, str);
  }

  // If there's no socket, always report full success if anything got out.
  return debuglog ? static_cast<int>(str.size()) : -1;
}

void DiagnosticsReport::Printf(const char* format, va_list args) const {
  __llvm_libc::printf_core::Printf<kBufferSize, __llvm_libc::printf_core::PrintfNewline::kYes>(
      startup_.LogClosure(), format, args);
}

template <>
void DiagnosticsReport::ReportModuleLoaded<StartupModule>(const StartupModule& module) const {
  if (startup_.ld_debug) {
    ModuleSymbolizerContext<kBufferSize>(startup_.LogClosure(), module);
  }
}

}  // namespace ld
