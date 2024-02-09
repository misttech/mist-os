// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/util.h"

#include <lib/syslog/cpp/macros.h>

namespace tracing {

std::ostream& operator<<(std::ostream& out, TransferStatus status) {
  switch (status) {
    case TransferStatus::kComplete:
      out << "complete";
      break;
    case TransferStatus::kProviderError:
      out << "provider error";
      break;
    case TransferStatus::kWriteError:
      out << "write error";
      break;
    case TransferStatus::kReceiverDead:
      out << "receiver dead";
      break;
  }

  return out;
}

std::ostream& operator<<(std::ostream& out, fuchsia::tracing::BufferDisposition disposition) {
  switch (disposition) {
    case fuchsia::tracing::BufferDisposition::CLEAR_ENTIRE:
      out << "clear-all";
      break;
    case fuchsia::tracing::BufferDisposition::CLEAR_NONDURABLE:
      out << "clear-nondurable";
      break;
    case fuchsia::tracing::BufferDisposition::RETAIN:
      out << "retain";
      break;
  }

  return out;
}

std::ostream& operator<<(std::ostream& out, controller::SessionState state) {
  switch (state) {
    case controller::SessionState::READY:
      out << "ready";
      break;
    case controller::SessionState::INITIALIZED:
      out << "initialized";
      break;
    case controller::SessionState::STARTING:
      out << "starting";
      break;
    case controller::SessionState::STARTED:
      out << "started";
      break;
    case controller::SessionState::STOPPING:
      out << "stopping";
      break;
    case controller::SessionState::STOPPED:
      out << "stopped";
      break;
    case controller::SessionState::TERMINATING:
      out << "terminaing";
      break;
  }

  return out;
}

}  // namespace tracing
