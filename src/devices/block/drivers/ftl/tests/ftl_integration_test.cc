// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.storage.ftl/cpp/fidl.h>
#include <fidl/fuchsia.storage.ftl/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zxio/include/lib/zxio/types.h>
#include <lib/zxio/include/lib/zxio/zxio.h>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.storage.ftl/cpp/markers.h"
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
  auto [service_client, service_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_EQ(fdio_service_connect("/driver_exposed/fuchsia.storage.ftl.Service",
                                 service_server.channel().release()),
            ZX_OK);

  // The service will be a random instance name string, find it in the listing. This would be racy
  // if the test environment didn't begin by waiting for the block device to be available, which
  // happens after registering the service.
  zxio_storage_t io_storage;
  ASSERT_EQ(zxio_create(service_client.channel().get(), &io_storage), ZX_OK);
  zxio_dirent_iterator_t iterator;
  ASSERT_EQ(zxio_dirent_iterator_init(&iterator, &io_storage.io), ZX_OK);
  char name[255] = ".";
  zxio_dirent_t dirent;
  dirent.name = name;
  dirent.name_length = 1;
  // Should only be one entry excluding the "." entry.
  while (name[0] == '.' && name[1] == '\0') {
    ASSERT_EQ(zxio_dirent_iterator_next(&iterator, &dirent), ZX_OK);
    name[dirent.name_length] = '\0';
  }

  // Connect to the child randomly named instance.
  auto [instance_client, instance_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_EQ(fdio_service_connect_at(service_client.channel().get(), name,
                                    instance_server.channel().release()),
            ZX_OK);

  // Connect to the Configuration protocol inside the service.
  auto [config_client, config_server] = fidl::Endpoints<fsftl::Configuration>::Create();
  ASSERT_EQ(fdio_service_connect_at(instance_client.channel().get(), fsftl::Service::Config::Name,
                                    config_server.channel().release()),
            ZX_OK);
  out_client->Bind(std::move(config_client));
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
