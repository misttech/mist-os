// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/fdio/spawn.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/process.h>
#include <zircon/process.h>
#include <zircon/processargs.h>

#include "src/developer/sshd-host/service.h"

const uint16_t kPort = 22;
const char* kKeyGenArgs[] = {"/pkg/bin/hostkeygen", nullptr};

int main(int argc, const char** argv) {
  fuchsia_logging::SetTags({"sshd-host"});
  // We need to close PA_DIRECTORY_REQUEST otherwise clients that expect us to
  // offer services won't know that we've started and are not going to offer
  // any services.
  //
  // TODO(abarth): Instead of closing this handle, we should offer some
  // introspection services for debugging.
  { zx::handle((zx_take_startup_handle(PA_DIRECTORY_REQUEST))); }

  std::shared_ptr service_directory = sys::ComponentContext::Create()->svc();

  FX_SLOG(INFO, "sshd-host starting up");

  if (zx_status_t status =
          sshd_host::provision_authorized_keys_from_bootloader_file(service_directory);
      status != ZX_OK) {
    FX_SLOG(WARNING, "Failed to provision authorized_keys",
            KV("status", zx_status_get_string(status)));
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  zx::process process;
  if (zx_status_t status = fdio_spawn(0, FDIO_SPAWN_CLONE_ALL, kKeyGenArgs[0], kKeyGenArgs,
                                      process.reset_and_get_address());
      status != ZX_OK) {
    FX_SLOG(FATAL, "Failed to spawn hostkeygen", KV("status", zx_status_get_string(status)));
  }
  if (zx_status_t status = process.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), nullptr);
      status != ZX_OK) {
    FX_SLOG(FATAL, "Failed to wait for hostkeygen", KV("status", zx_status_get_string(status)));
  }
  zx_info_process_t info;
  if (zx_status_t status = process.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    FX_SLOG(FATAL, "Failed to get hostkeygen info", KV("status", zx_status_get_string(status)));
  }

  if (info.return_code != 0) {
    FX_SLOG(FATAL, "Failed to generate host keys", KV("return_code", info.return_code));
  }

  uint16_t port = kPort;
  if (argc > 1) {
    int arg = atoi(argv[1]);
    if (arg <= 0) {
      FX_SLOG(ERROR, "Invalid port", KV("argv[1]", argv[1]));
      return -1;
    }
    port = static_cast<uint16_t>(arg);
  }
  sshd_host::Service service(loop.dispatcher(), std::move(service_directory), port);

  if (zx_status_t status = loop.Run(); status != ZX_OK) {
    FX_SLOG(FATAL, "Failed to run loop", KV("status", zx_status_get_string(status)));
  }

  return 0;
}
