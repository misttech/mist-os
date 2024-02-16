// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/client/analytics.h"

#include "src/developer/debug/zxdb/client/setting_schema_definition.h"

namespace zxdb {

using ::analytics::core_dev_tools::AnalyticsOption;
using ::analytics::core_dev_tools::InvokeEvent;

void Analytics::Init(Session& session, AnalyticsOption analytics_option) {
  InitBotAware(analytics_option, false);
  session.system().settings().SetBool(ClientSettings::System::kEnableAnalytics, enabled_runtime_);
}

bool Analytics::IsEnabled(Session* session) {
  return !ClientIsCleanedUp() &&
         session->system().settings().GetBool(ClientSettings::System::kEnableAnalytics);
}

void Analytics::IfEnabledSendInvokeEvent(Session* session) {
  if (IsEnabled(session)) {
    SendGa4Event(std::make_unique<InvokeEvent>());
  }
}

}  // namespace zxdb
