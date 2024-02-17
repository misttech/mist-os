// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/client/analytics_event.h"

namespace zxdb {

// AnalyticsEvent ----------------------------------------------------------------------------------
AnalyticsEvent::AnalyticsEvent(const std::string& name, const std::string& session_id)
    : analytics::core_dev_tools::Ga4Event(name) {
  // Note: this is a special name, and is processed by analytics differently than typical custom
  // parameters. This surfaces in the data as "ga_session_id", which (should) associate it with
  // other built-in session metrics.
  SetParameter("session_id", session_id);
}

// SessionStarted ----------------------------------------------------------------------------------
SessionStarted::SessionStarted(const std::string& session_id)
    : AnalyticsEvent("session_started", session_id) {}

SessionConnected::SessionConnected(const std::string& session_id)
    : AnalyticsEvent("session_connected", session_id) {}

// SessionConnected --------------------------------------------------------------------------------
void SessionConnected::SetRemoteType(SessionConnected::RemoteType type) {
  std::string remote_type_string;
  switch (type) {
    case RemoteType::kRemoteAgent:
      remote_type_string = "remote agent";
      break;
    case RemoteType::kLocalAgent:
      remote_type_string = "local agent";
      break;
    case RemoteType::kMinidump:
      remote_type_string = "minidump";
      break;
  }

  SetParameter("remote_type", remote_type_string);
}

// SessionEnded ------------------------------------------------------------------------------------
SessionEnded::SessionEnded(const std::string& session_id)
    : AnalyticsEvent("session_ended", session_id) {}

void SessionEnded::SetSessionTime(std::chrono::milliseconds session_time) {
  SetParameter("session_length_ms", session_time.count());
}

}  // namespace zxdb
