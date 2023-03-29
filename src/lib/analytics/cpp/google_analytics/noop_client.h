// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ANALYTICS_CPP_GOOGLE_ANALYTICS_NOOP_CLIENT_H_
#define SRC_LIB_ANALYTICS_CPP_GOOGLE_ANALYTICS_NOOP_CLIENT_H_

#include "src/lib/analytics/cpp/google_analytics/client.h"

namespace analytics::google_analytics {

// No-Op UA client for testing only.
// The GA4 has a real testing client but here for UA we only provide a no-op one
// because UA is deprecated.
class NoOpClient : public Client {
 public:
  NoOpClient() {
    SetUserAgent("noop-agent");
    SetTrackingId("noop-tid");
    SetClientId("noop-cid");
  }

 private:
  void SendData(std::string_view user_agent,
                std::map<std::string, std::string> parameters) override {}
};

}  // namespace analytics::google_analytics

#endif  // SRC_LIB_ANALYTICS_CPP_GOOGLE_ANALYTICS_NOOP_CLIENT_H_
