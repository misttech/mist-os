// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SYSTEM_ACTIVITY_GOVERNOR_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SYSTEM_ACTIVITY_GOVERNOR_H_

#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/syslog/cpp/macros.h>

#include "src/developer/forensics/testing/stubs/fidl_server.h"

namespace forensics::stubs {

class SystemActivityGovernor : public FidlServer<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernor(fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> server_end,
                         async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this, &SystemActivityGovernor::OnFidlClosed) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override;

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

 private:
  fidl::ServerBinding<fuchsia_power_system::ActivityGovernor> binding_;
};

class SystemActivityGovernorNoPowerElements
    : public FidlServer<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernorNoPowerElements(
      fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> server_end,
      async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this,
                 &SystemActivityGovernorNoPowerElements::OnFidlClosed) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    completer.Reply(fidl::Response<fuchsia_power_system::ActivityGovernor::GetPowerElements>());
  }

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

 private:
  fidl::ServerBinding<fuchsia_power_system::ActivityGovernor> binding_;
};

class SystemActivityGovernorNoTokens : public FidlServer<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernorNoTokens(fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> server_end,
                                 async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this,
                 &SystemActivityGovernorNoTokens::OnFidlClosed) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override;

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

 private:
  fidl::ServerBinding<fuchsia_power_system::ActivityGovernor> binding_;
};

class SystemActivityGovernorClosesConnection
    : public FidlServer<fuchsia_power_system::ActivityGovernor> {
 public:
  SystemActivityGovernorClosesConnection(
      fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> server_end,
      async_dispatcher_t* dispatcher)
      : binding_(dispatcher, std::move(server_end), this,
                 &SystemActivityGovernorClosesConnection::OnFidlClosed) {}

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_PEER_CLOSED);
  }

  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

 private:
  fidl::ServerBinding<fuchsia_power_system::ActivityGovernor> binding_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SYSTEM_ACTIVITY_GOVERNOR_H_
