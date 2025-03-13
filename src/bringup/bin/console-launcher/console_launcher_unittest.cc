// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/bringup/bin/console-launcher/console_launcher.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <mock-boot-arguments/server.h>
#include <zxtest/zxtest.h>

namespace {

TEST(SystemInstanceTest, CheckBootArgParsing) {
  std::map<std::string, std::string> arguments;
  arguments["console.shell"] = "true";
  arguments["console.use_virtio_console"] = "true";
  arguments["TERM"] = "FAKE_TERM";
  arguments["zircon.autorun.boot"] = "/boot/bin/ls+/dev/class/";
  arguments["zircon.autorun.system"] = "/boot/bin/ls+/system";

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_TRUE(args->run_shell);
  ASSERT_EQ(args->term, "TERM=FAKE_TERM");
  ASSERT_TRUE(args->use_virtio_console);
  ASSERT_EQ(args->autorun_boot, "/boot/bin/ls+/dev/class/");
  ASSERT_EQ(args->autorun_system, "/boot/bin/ls+/system");
  ASSERT_EQ(args->virtcon_disabled, false);
}

TEST(SystemInstanceTest, CheckBootArgDefaultStrings) {
  std::map<std::string, std::string> arguments;

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_FALSE(args->run_shell);
  ASSERT_EQ(args->term, "TERM=uart");
  ASSERT_FALSE(args->use_virtio_console);
  ASSERT_EQ(args->autorun_boot, "");
  ASSERT_EQ(args->autorun_system, "");
}

// The defaults are that a system is not required, so zedboot will try to launch.
TEST(VirtconSetup, VirtconDefaults) {
  std::map<std::string, std::string> arguments;

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_FALSE(args->virtual_console_need_debuglog);
}

// Need debuglog should be true when netboot is true and netboot is not disabled.
TEST(VirtconSetup, VirtconNeedDebuglog) {
  std::map<std::string, std::string> arguments;
  arguments["netsvc.disable"] = "false";
  arguments["netsvc.netboot"] = "true";

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_TRUE(args->virtual_console_need_debuglog);
}

// If netboot is true but netsvc is disabled, don't start debuglog.
TEST(VirtconSetup, VirtconNetbootWithNetsvcDisabled) {
  std::map<std::string, std::string> arguments;
  arguments["netsvc.disable"] = "true";
  arguments["netsvc.netboot"] = "true";

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_FALSE(args->virtual_console_need_debuglog);
}

// Check that virtcon_disabled is propogated through to args correctly.
TEST(VirtconSetup, VirtconDisabled) {
  std::map<std::string, std::string> arguments;

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  mock_boot_arguments::Server boot_server(std::move(arguments));
  loop.StartThread();

  zx::result boot_args = boot_server.CreateClient(loop.dispatcher());
  ASSERT_OK(boot_args);

  console_launcher_config::Config config;
  config.virtcon_disabled() = true;
  zx::result args = console_launcher::GetArguments(boot_args.value().client_end(), config);
  ASSERT_OK(args.status_value());

  ASSERT_TRUE(args->virtcon_disabled);
}

}  // namespace
