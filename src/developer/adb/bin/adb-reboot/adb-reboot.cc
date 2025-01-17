// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-reboot.h"

#include <fidl/fuchsia.hardware.adb/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <stdio.h>
#include <zircon/errors.h>
#include <zircon/types.h>

namespace adb_reboot {

void AdbReboot::ConnectToService(ConnectToServiceRequestView request,
                                 ConnectToServiceCompleter::Sync& completer) {
  auto status = Reboot({std::string(request->args.get())});
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess();
}

zx_status_t AdbReboot::Reboot(std::optional<std::string> args) {
  zx_status_t status = ZX_OK;

  if (args->empty() || args.value().empty()) {
    FX_LOGS(INFO) << "adb reboot";
    status =
        ScheduleReboot([](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          fidl::Arena arena;
          auto builder = fuchsia_hardware_power_statecontrol::wire::RebootOptions::Builder(arena);
          fuchsia_hardware_power_statecontrol::RebootReason2 reasons[1] = {
              fuchsia_hardware_power_statecontrol::RebootReason2::kUserRequest};
          auto vector_view =
              fidl::VectorView<fuchsia_hardware_power_statecontrol::RebootReason2>::FromExternal(
                  reasons);
          builder.reasons(vector_view);
          return client->PerformReboot(builder.Build());
        });

  } else if (args.value() == "bootloader") {
    FX_LOGS(INFO) << "adb reboot bootloader";
    status =
        ScheduleReboot([](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->RebootToBootloader();
        });
  } else if (args.value() == "recovery") {
    FX_LOGS(INFO) << "adb reboot recovery";
    status =
        ScheduleReboot([](fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> client) {
          return client->RebootToRecovery();
        });

  } else {
    FX_LOGS(ERROR) << "Error: Invalid args for adb reboot: " << args.value();
    status = ZX_ERR_INVALID_ARGS;
  }

  return status;
}

template <typename RebootFunction>
zx_status_t AdbReboot::ScheduleReboot(RebootFunction f) {
  auto client_end = component::ConnectAt<fuchsia_hardware_power_statecontrol::Admin>(svc_);
  if (client_end.is_error()) {
    FX_LOGS(ERROR) << "Could not connect to hardware power state control: "
                   << client_end.status_string();
    return client_end.status_value();
  }

  fidl::WireSyncClient<fuchsia_hardware_power_statecontrol::Admin> reboot_client(
      std::move(*client_end));

  // Post task on dispatcher so that reboot occurs after replying to adb command.
  return async::PostTask(dispatcher_, [&, client = std::move(reboot_client)]() mutable {
    auto response = f(std::move(client));
    if (response.status() != ZX_OK) {
      // Note: This warning might be triggered in tests when the realm builder shuts down power
      // statecontrol instance before the adb-reboot instance, which is fine.
      FX_LOGS(WARNING) << "Reboot: " << response.status_string();
    } else if (response->is_error()) {
      FX_LOGS(WARNING) << "Reboot: " << response->error_value();
    }
  });
}

}  // namespace adb_reboot
