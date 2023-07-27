// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.hardware.btitest/cpp/wire.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/time.h>
#include <zircon/status.h>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

namespace {

using device_watcher::RecursiveWaitForFile;

using namespace component_testing;

constexpr char kParentPath[] = "sys/platform/11:01:1a";
constexpr char kDeviceName[] = "test-bti";

TEST(PbusBtiTest, BtiIsSameAfterCrash) {
  auto realm_builder = component_testing::RealmBuilder::Create();
  driver_test_realm::Setup(realm_builder);
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.boot.RootResource"}},
      .source = {ParentRef()},
      .targets = {ChildRef{"driver_test_realm"}},
  });

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto realm = realm_builder.Build(loop.dispatcher());

  // Start DriverTestRealm.
  fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
  ASSERT_EQ(ZX_OK, realm.component().Connect(driver_test_realm.NewRequest()));
  fuchsia::driver::test::Realm_Start_Result realm_result;
  fuchsia::driver::test::RealmArgs args;
  args.set_root_driver("fuchsia-boot:///platform-bus#meta/platform-bus.cm");
  ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
  ASSERT_FALSE(realm_result.is_err());

  // Connect to the parent directory.
  fbl::unique_fd parent_dir;
  {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    ASSERT_OK(realm.component().Connect("dev-topological", endpoints->server.TakeChannel()));
    fbl::unique_fd dev_fd;
    ASSERT_OK(
        fdio_fd_create(endpoints->client.TakeChannel().release(), dev_fd.reset_and_get_address()));
    ASSERT_OK(RecursiveWaitForFile(dev_fd.get(), kParentPath));
    ASSERT_OK(fdio_open_fd_at(dev_fd.get(), kParentPath,
                              static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory),
                              parent_dir.reset_and_get_address()));
  }

  uint64_t koid1;
  {
    fidl::WireSyncClient<fuchsia_hardware_btitest::BtiDevice> client;
    {
      zx::result channel = RecursiveWaitForFile(parent_dir.get(), kDeviceName);
      ASSERT_OK(channel);
      client.Bind(fidl::ClientEnd<fuchsia_hardware_btitest::BtiDevice>(std::move(channel.value())));
    }
    {
      const fidl::WireResult result = client->GetKoid();
      ASSERT_OK(result.status());
      koid1 = result.value().koid;
    }

    zx::result dir_watcher =
        device_watcher::DirWatcher::Create(fdio_cpp::UnownedFdioCaller(parent_dir).directory());
    ASSERT_OK(dir_watcher);

    ASSERT_OK(client->Crash());
    // We have to wait for both the entry to be removed in devfs and for the channel to be
    // closed. The channel closes before the device is removed from devfs so only waiting for
    // one could result in a race.
    ASSERT_OK(dir_watcher->WaitForRemoval(kDeviceName, zx::duration::infinite()));
    ASSERT_OK(client.client_end().channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(),
                                                     nullptr));
  }

  // We implicitly rely on driver host being rebound in the event of a crash.
  uint64_t koid2;
  {
    fidl::WireSyncClient<fuchsia_hardware_btitest::BtiDevice> client;
    {
      zx::result channel = RecursiveWaitForFile(parent_dir.get(), kDeviceName);
      ASSERT_OK(channel);
      client.Bind(fidl::ClientEnd<fuchsia_hardware_btitest::BtiDevice>(std::move(channel.value())));
    }
    {
      const fidl::WireResult result = client->GetKoid();
      ASSERT_OK(result.status());
      koid2 = result.value().koid;
    }
  }

  ASSERT_EQ(koid1, koid2);
}

}  // namespace
