// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.storage.ftl/cpp/fidl.h>
#include <fidl/fuchsia.storage.ftl/cpp/wire.h>
#include <lib/component/incoming/cpp/service_member_watcher.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>

#include <zxtest/zxtest.h>

#include "ftl_test_observer.h"
#include "launch.h"

namespace {

namespace fsftl = fuchsia_storage_ftl;

TEST(FtlTest, BlockTest) {
  const char* argv[] = {"/pkg/bin/blktest", "-d", kTestDevice, nullptr};

  ASSERT_EQ(0, Execute(argv));
}

TEST(FtlTest, IoCheck) {
  const char* argv[] = {"/pkg/bin/iochk", "-bs",  "32k", "--live-dangerously", "-t", "2",
                        kTestDevice,      nullptr};

  ASSERT_EQ(0, Execute(argv));
}

void ConnectToConfiguration(fidl::WireSyncClient<fsftl::Configuration>* out_client) {
  auto [svc, svc_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_EQ(fdio_open3("/driver_exposed", uint64_t{fuchsia_io::wire::kPermReadable},
                       svc_server.channel().release()),
            ZX_OK);

  component::SyncServiceMemberWatcher<fsftl::Service::Config> watcher(svc);
  zx::result config = watcher.GetNextInstance(false);
  ASSERT_STATUS(config.status_value(), ZX_OK);

  out_client->Bind(std::move(config.value()));
}

TEST(FtlTest, ConfigurationService) {
  fidl::WireSyncClient<fsftl::Configuration> client;
  ASSERT_NO_FATAL_FAILURE(ConnectToConfiguration(&client));

  // The feature begins disabled.
  {
    fidl::WireResult<fsftl::Configuration::Get> res = client->Get();
    ASSERT_TRUE(res->is_ok());
    ASSERT_TRUE(res.value()->has_use_new_wear_leveling());
    ASSERT_FALSE(res.value()->use_new_wear_leveling());
  }

  // Flip it on and verify.
  {
    fidl::WireTableFrame<fsftl::wire::ConfigurationOptions> options_frame;
    auto options = fsftl::wire::ConfigurationOptions::ExternalBuilder(
        fidl::ObjectView<fidl::WireTableFrame<fsftl::wire::ConfigurationOptions>>::FromExternal(
            &options_frame));
    options.use_new_wear_leveling(true);
    fidl::WireResult<fsftl::Configuration::Set> res = client->Set(options.Build());
    ASSERT_TRUE(res->is_ok());
  }
  {
    fidl::WireResult<fsftl::Configuration::Get> res = client->Get();
    ASSERT_TRUE(res->is_ok());
    ASSERT_TRUE(res.value()->has_use_new_wear_leveling());
    ASSERT_TRUE(res.value()->use_new_wear_leveling());
  }

  // Flip it back off and verify.
  {
    fidl::WireTableFrame<fsftl::wire::ConfigurationOptions> options_frame;
    auto options = fsftl::wire::ConfigurationOptions::ExternalBuilder(
        fidl::ObjectView<fidl::WireTableFrame<fsftl::wire::ConfigurationOptions>>::FromExternal(
            &options_frame));
    options.use_new_wear_leveling(false);
    fidl::WireResult<fsftl::Configuration::Set> res = client->Set(options.Build());
    ASSERT_TRUE(res->is_ok());
  }
  {
    fidl::WireResult<fsftl::Configuration::Get> res = client->Get();
    ASSERT_TRUE(res->is_ok());
    ASSERT_TRUE(res.value()->has_use_new_wear_leveling());
    ASSERT_FALSE(res.value()->use_new_wear_leveling());
  }
}

}  // namespace
