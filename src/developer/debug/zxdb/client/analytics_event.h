// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_ANALYTICS_EVENT_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_ANALYTICS_EVENT_H_

#include <chrono>

#include "src/lib/analytics/cpp/core_dev_tools/google_analytics_4_client.h"

namespace zxdb {

// Base class for all zxdb analytics events. All events are associated with a session identifier,
// and the analytics library automatically adds a client/user identifier to all events.
//
// Derived classes should include methods to set individual parameters specific for that event.
// Details about the limits on events can be found here
// https://support.google.com/analytics/answer/12229021 and here
// https://support.google.com/analytics/answer/9267744?sjid=13155358058205528748-NC. Parameter types
// must be convertible to the valid types described for the base Ga4Event class.
class AnalyticsEvent : public analytics::core_dev_tools::Ga4Event {
 public:
  AnalyticsEvent(const std::string& name, const std::string& session_id);
};

// This is distinct from the analytics default Invoke event, since this initializes this session_id.
// This is issued upon any connection attempt after the session is constructed (we can't send the
// event during Session construction because that happens before the analytics backend is
// initialized, and that needs the session).
class SessionStarted final : public AnalyticsEvent {
 public:
  explicit SessionStarted(const std::string& session_id);
};

// Issued when the session is connected to a RemoteAPI implementation.
class SessionConnected final : public AnalyticsEvent {
 public:
  explicit SessionConnected(const std::string& session_id);

  enum class RemoteType {
    kRemoteAgent,  // Connected to a remote DebugAgent.
    kLocalAgent,   // Connected to a local DebugAgent.
    kMinidump,     // Connected to MinidumpRemoteAPI.
  };

  void SetRemoteType(RemoteType type);
};

// Issued when the Session is destructed.
class SessionEnded final : public AnalyticsEvent {
 public:
  explicit SessionEnded(const std::string& session_id);

  void SetSessionTime(std::chrono::milliseconds session_time);
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_ANALYTICS_EVENT_H_
