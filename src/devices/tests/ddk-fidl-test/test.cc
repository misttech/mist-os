// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.hardware.test/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/unsafe.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/fidl.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <vector>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.hardware.test/cpp/wire_messaging.h"

using driver_integration_test::IsolatedDevmgr;

namespace {

void CheckTransaction(const board_test::DeviceEntry& entry, const char* device_fs) {
  IsolatedDevmgr devmgr;

  // Set the driver arguments.
  IsolatedDevmgr::Args args;
  args.device_list.push_back(entry);

  // Create the isolated Devmgr.
  zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr);
  ASSERT_OK(status);

  // Wait for the driver to be created
  zx::result channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), device_fs);
  ASSERT_OK(channel.status_value());
  fidl::WireSyncClient client{
      fidl::ClientEnd<fuchsia_hardware_test::Device>(std::move(channel.value()))};

  // If the transaction incorrectly closes the sent handles, it will cause a policy violation.
  // Calling the API once isn't enough, there is still a very small amount of time before the
  // transaction destructor runs. A second call ensures that the first succeeded. If a policy
  // violation occurs, the second call below will fail as the driver channel will have been closed.
  for (uint32_t i = 0; i < 2; i++) {
    fidl::WireResult result = client->GetChannel();
    ASSERT_OK(result.status());
  }
}

// Test that the transaction does not incorrectly close handles during Reply.
TEST(FidlDDKDispatcherTest, SyncTransactionHandleTest) {
  board_test::DeviceEntry entry = {};
  strcpy(entry.name, "ddk-fidl");
  entry.vid = PDEV_VID_TEST;
  entry.pid = PDEV_PID_DDKFIDL_TEST;
  entry.did = PDEV_DID_TEST_DDKFIDL;
  CheckTransaction(entry, "sys/platform/11:09:d/ddk-fidl");
}

TEST(FidlDDKDispatcherTest, AsyncTransactionHandleTest) {
  board_test::DeviceEntry entry = {};
  strcpy(entry.name, "ddk-async-fidl");
  entry.vid = PDEV_VID_TEST;
  entry.pid = PDEV_PID_DDKFIDL_TEST;
  entry.did = PDEV_DID_TEST_DDKASYNCFIDL;
  CheckTransaction(entry, "sys/platform/11:09:15/ddk-async-fidl");
}

}  // namespace
