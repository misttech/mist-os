// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/files/file.h"
#include "src/lib/fsl/io/fd.h"
#include "src/modular/bin/basemgr/basemgr_impl_test_fixture.h"

namespace modular {

class BasemgrImplTest : public BasemgrImplTestFixture {};

// Tests that basemgr starts a session with the given configuration when instructed by
// the session launcher component.
TEST_F(BasemgrImplTest, StartsSessionWithConfig) {
  static constexpr auto kTestSessionShellUrl =
      "fuchsia-pkg://fuchsia.com/test_session_shell#meta/test_session_shell.cmx";

  FakeSessionmgr sessionmgr{fake_launcher_};

  CreateBasemgrImpl(DefaultConfig());

  // Create the configuration that the session launcher component passes to basemgr.
  fuchsia::modular::session::SessionShellMapEntry entry;
  entry.mutable_config()->mutable_app_config()->set_url(kTestSessionShellUrl);
  entry.mutable_config()->mutable_app_config()->set_args({});
  fuchsia::modular::session::ModularConfig config;
  config.mutable_basemgr_config()->mutable_session_shell_map()->push_back(std::move(entry));
  auto config_json = modular::ConfigToJsonString(config);

  LaunchSessionmgr(std::move(config));

  // sessionmgr should be started and initialized.
  RunLoopUntil([&]() { return sessionmgr.initialized(); });

  // sessionmgr's namespace should contain the config file at /config_override/data/startup.config.
  auto config_dir_it =
      sessionmgr.component()->namespace_map().find(modular_config::kOverriddenConfigDir);
  ASSERT_TRUE(config_dir_it != sessionmgr.component()->namespace_map().end());

  auto did_read_config = false;
  ASSERT_EQ(ZX_OK, loop().StartThread());
  async::TaskClosure task([&] {
    auto dir_fd = fsl::OpenChannelAsFileDescriptor(config_dir_it->second.TakeChannel());

    std::string config_contents;
    ASSERT_TRUE(files::ReadFileToStringAt(dir_fd.get(), modular_config::kStartupConfigFilePath,
                                          &config_contents));

    EXPECT_EQ(config_json, config_contents);

    did_read_config = true;
  });
  task.Post(dispatcher());

  RunLoopUntil([&]() { return did_read_config; });
  loop().JoinThreads();

  basemgr_impl_->Stop();
  RunLoopUntil([&]() { return did_shut_down_; });
}

// Tests that basemgr starts a new sessionmgr component with a new configuration when instructed
// to launch a new session.
TEST_F(BasemgrImplTest, LaunchSessionmgrReplacesExistingSession) {
  FakeSessionmgr sessionmgr{fake_launcher_};

  CreateBasemgrImpl(DefaultConfig());
  LaunchSessionmgr(DefaultConfig());

  // sessionmgr should be started and initialized.
  RunLoopUntil([&]() { return sessionmgr.initialized(); });

  EXPECT_EQ(1, sessionmgr.component()->launch_count());

  // Launch the session again
  LaunchSessionmgr(DefaultConfig());

  RunLoopUntil([&] { return sessionmgr.component()->launch_count() == 2; });

  basemgr_impl_->Stop();
  RunLoopUntil([&]() { return did_shut_down_; });
}

// Tests that basemgr waits for sessionmgr to terminate before itself exiting.
TEST_F(BasemgrImplTest, WaitsForSessionmgrShutdown) {
  bool did_shut_down_sessionmgr{false};
  FakeSessionmgr sessionmgr{fake_launcher_, [&] { did_shut_down_sessionmgr = true; }};

  CreateBasemgrImpl(DefaultConfig());
  LaunchSessionmgr(DefaultConfig());

  // sessionmgr should be started and initialized.
  RunLoopUntil([&]() { return sessionmgr.initialized(); });

  EXPECT_EQ(1, sessionmgr.component()->launch_count());

  LaunchSessionmgr(DefaultConfig());

  RunLoopUntil([&] { return sessionmgr.component()->launch_count() == 2; });

  basemgr_impl_->Stop();
  RunLoopUntil([&]() { return did_shut_down_sessionmgr; });
  // basemgr should not shut down until sessionmgr's component has actually
  // terminated.
  EXPECT_FALSE(did_shut_down_);
  sessionmgr.component()->CloseAllComponentControllerHandles();
  RunLoopUntil([&]() { return did_shut_down_; });
}

}  // namespace modular
