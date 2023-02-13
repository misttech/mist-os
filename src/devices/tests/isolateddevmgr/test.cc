// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.manager.test/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/syscalls.h>

#include <string>
#include <vector>

#include <zxtest/zxtest.h>

using driver_integration_test::IsolatedDevmgr;

namespace {

class IsolatedDevMgrTest : public zxtest::Test {};

const std::vector<uint8_t> metadata1 = {1, 2, 3, 4, 5};
const board_test::DeviceEntry kDeviceEntry1 = []() {
  board_test::DeviceEntry entry = {};
  strcpy(entry.name, "metadata-test");
  entry.vid = PDEV_VID_TEST;
  entry.pid = PDEV_PID_METADATA_TEST;
  entry.did = PDEV_DID_TEST_CHILD_1;
  entry.metadata_size = metadata1.size();
  entry.metadata = metadata1.data();
  return entry;
}();

const std::vector<uint8_t> metadata2 = {7, 6, 5, 4, 3, 2, 1};
const board_test::DeviceEntry kDeviceEntry2 = []() {
  board_test::DeviceEntry entry = {};
  strcpy(entry.name, "metadata-test");
  entry.vid = PDEV_VID_TEST;
  entry.pid = PDEV_PID_METADATA_TEST;
  entry.did = PDEV_DID_TEST_CHILD_2;
  entry.metadata_size = metadata2.size();
  entry.metadata = metadata2.data();
  return entry;
}();

void CheckMetadata(fidl::WireSyncClient<fuchsia_device_manager_test::Metadata>& client,
                   const std::vector<uint8_t>& expected_metadata) {
  fidl::WireResult result = client->GetMetadata(DEVICE_METADATA_TEST);
  ASSERT_OK(result.status());
  fidl::VectorView<uint8_t> received_metadata = std::move(result->data);
  ASSERT_EQ(received_metadata.count(), expected_metadata.size());

  EXPECT_BYTES_EQ(received_metadata.data(), expected_metadata.data(), expected_metadata.size());
}

TEST_F(IsolatedDevMgrTest, MetadataOneDriverTest) {
  IsolatedDevmgr devmgr;

  // Set the driver arguments.
  IsolatedDevmgr::Args args;
  args.device_list.push_back(kDeviceEntry1);

  // Create the isolated Devmgr.
  zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr);
  ASSERT_OK(status);

  // Wait for Metadata-test driver to be created
  zx::result channel = device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(),
                                                            "sys/platform/11:07:2/metadata-test");
  ASSERT_OK(channel.status_value());

  fidl::WireSyncClient client{
      fidl::ClientEnd<fuchsia_device_manager_test::Metadata>(std::move(channel.value()))};
  ASSERT_NO_FATAL_FAILURE(CheckMetadata(client, metadata1));
}

TEST_F(IsolatedDevMgrTest, MetadataTwoDriverTest) {
  IsolatedDevmgr devmgr;

  // Set the driver arguments.
  IsolatedDevmgr::Args args;
  args.device_list.push_back(kDeviceEntry1);
  args.device_list.push_back(kDeviceEntry2);

  // Create the isolated Devmgr.
  zx_status_t status = IsolatedDevmgr::Create(&args, &devmgr);
  ASSERT_OK(status);

  struct MetadataTest {
    std::string path;
    std::vector<uint8_t> metadata;
  };

  std::vector<MetadataTest> tests{
      {
          .path = "sys/platform/11:07:2/metadata-test",
          .metadata = metadata1,
      },
      {
          .path = "sys/platform/11:07:3/metadata-test",
          .metadata = metadata2,
      },
  };

  for (MetadataTest& test : tests) {
    SCOPED_TRACE(test.path);
    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr.devfs_root().get(), test.path.c_str());
    ASSERT_OK(channel.status_value());
    fidl::WireSyncClient client{
        fidl::ClientEnd<fuchsia_device_manager_test::Metadata>(std::move(channel.value()))};
    ASSERT_NO_FATAL_FAILURE(CheckMetadata(client, test.metadata));
  }
}

}  // namespace
