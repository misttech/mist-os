// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <stdio.h>
#include <zircon/status.h>

#include <filesystem>

#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

namespace {
const std::string kSysinfoServiceDir = "/svc/fuchsia.sysinfo.Service";
std::string FindFirstInstance() {
  for (const auto& entry : std::filesystem::directory_iterator(kSysinfoServiceDir)) {
    if (entry.path().string() != ".") {
      return entry.path().string();
    }
  };
  return "";
}
}  // namespace

int main(int argc, const char** argv) {
  if (const zx_status_t status = StdoutToDebuglog::Init(); status != ZX_OK) {
    fprintf(stderr, "sysinfo: StdoutToDebuglog::Init() = %s\n", zx_status_get_string(status));
    return -1;
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());
  if (const zx::result status = outgoing.AddUnmanagedProtocol<fuchsia_sysinfo::SysInfo>(
          [](fidl::ServerEnd<fuchsia_sysinfo::SysInfo> server_end) {
            std::string first_instance = FindFirstInstance();
            if (first_instance == "") {
              fprintf(stderr, "sysinfo: no instances found");
            }
            std::string path = first_instance + "/device";

            if (const zx::result status =
                    component::Connect<fuchsia_sysinfo::SysInfo>(std::move(server_end), path);
                status.is_error()) {
              fprintf(stderr, "sysinfo: component::Connect(\"%s\") = %s\n", path.c_str(),
                      status.status_string());
            }
          });
      status.is_error()) {
    fprintf(stderr, "sysinfo: outgoing.AddProtocol() = %s\n", status.status_string());
    return -1;
  }

  if (const zx::result status = outgoing.ServeFromStartupInfo(); status.is_error()) {
    fprintf(stderr, "sysinfo: outgoing.ServeFromStartupInfo() = %s\n", status.status_string());
    return -1;
  }

  if (const zx_status_t status = loop.Run(); status != ZX_OK) {
    fprintf(stderr, "sysinfo: loop.Run() = %s\n", zx_status_get_string(status));
    return -1;
  }

  return 0;
}
