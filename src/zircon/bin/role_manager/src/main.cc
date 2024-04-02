// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>

#include "role.h"

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  zx::result create_result = RoleManager::Create();
  if (create_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to create role manager service: " << create_result.status_string();
    return -1;
  }
  std::unique_ptr<RoleManager> role_manager_service = std::move(create_result.value());

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);
  zx::result result =
      outgoing.AddProtocol<fuchsia_scheduler::RoleManager>(std::move(role_manager_service));
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add RoleManager protocol: " << result.status_string();
    return -1;
  }

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }
  FX_LOGS(INFO) << "Starting role manager\n";
  zx_status_t status = loop.Run();
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to run async loop: " << zx_status_get_string(status);
  }
  return 0;
}
