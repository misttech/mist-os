// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver_test_realm/src/boot_items.h>
#include <lib/driver_test_realm/src/root_job.h>
#include <lib/driver_test_realm/src/system_state.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include <sdk/lib/driver_test_realm/dtr_support_config.h>

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithDispatcher(loop.dispatcher()).BuildAndInitialize();

  async_dispatcher_t* dispatcher = loop.dispatcher();

  component::OutgoingDirectory outgoing(dispatcher);

  driver_test_realm::BootItems boot_items;
  driver_test_realm::SystemStateTransition system_state;
  driver_test_realm::RootJob root_job;

  auto config = dtr_support_config::Config::TakeFromStartupHandle();

  boot_items.SetBoardName(config.board_name());

  zx::result result = outgoing.AddUnmanagedProtocol<fuchsia_boot::Items>(
      [&boot_items, dispatcher, tunnel_boot_items = config.tunnel_boot_items()](
          fidl::ServerEnd<fuchsia_boot::Items> server_end) {
        auto result = boot_items.Serve(dispatcher, std::move(server_end), tunnel_boot_items);
        if (result.is_error()) {
          FX_LOGS(ERROR) << "Failed to tunnel fuchsia_boot::Items" << result.status_string();
        }
      });
  ZX_ASSERT(result.is_ok());

  result = outgoing.AddUnmanagedProtocol<fuchsia_system_state::SystemStateTransition>(
      system_state.CreateHandler(dispatcher));
  ZX_ASSERT(result.is_ok());

  result =
      outgoing.AddUnmanagedProtocol<fuchsia_kernel::RootJob>(root_job.CreateHandler(dispatcher));
  ZX_ASSERT(result.is_ok());

  result = outgoing.ServeFromStartupInfo();
  ZX_ASSERT(result.is_ok());

  loop.Run();
  return 0;
}
