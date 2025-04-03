// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/mount.h>

#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"

class NmfsTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (access("/sys/fs/fuchsia_network_monitor_fs", F_OK) == -1) {
      GTEST_SKIP() << "/sys/fs/fuchsia_network_monitor_fs not available, skipping...";
    }
    mount("fuchsia_network_monitor_fs", "/sys/fs/fuchsia_network_monitor_fs",
          "fuchsia_network_monitor_fs", 0, nullptr);
  }
};

TEST_F(NmfsTest, NmfsNetworkFileWriteFailure) {
  // Any string that is not the expected JSON format should not result in a write.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1", "test"))
      << "File contents must be in the proper JSON format";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1", "1"))
      << "File contents must be in the proper JSON format";

  // The name of the file must be an unsigned integer.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/test",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "File name must be parseable as an unsigned integer, was string";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1.0",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "File name must be parseable as an unsigned integer, was float";

  // The version must be a currently supported enum type.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V9999",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Version must match a currently supported version";

  // The netid must match the integer provided for the file name.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 2,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Network id must match the name of the file";

  // The netid must be a non-negative integer.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1.0,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Network id must be parseable as an unsigned integer, was float";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": -1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Network id must be parseable as an unsigned integer, was negative";

  // The mark must be a non-negative integer.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56.0,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Mark must be parseable as an unsigned integer, was float";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": -56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Mark must be parseable as an unsigned integer, was negative";

  // The handle must be a non-negative integer.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78.0,
    "dnsv4": [],
    "dnsv6": []
})")) << "Handle must be parseable as an unsigned integer, was float";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": -78,
    "dnsv4": [],
    "dnsv6": []
})")) << "Handle must be parseable as an unsigned integer, was negative";

  // The DNS fields must be arrays with correctly formatted addresses.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": ["foo"],
    "dnsv6": []
})")) << "DNS v4 addresses must be in the proper format";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": ["foo"]
})")) << "DNS v6 addresses must be in the proper format";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": "192.0.2.0",
    "dnsv6": []
})")) << "DNS v4 addresses must be provided in an array";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": "2001:db8::"
})")) << "DNS v6 addresses must be provided in an array";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": ["192.0.2.0"]
})")) << "DNS v4 addresses must be present in the v6 address list";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": ["2001:db8::"],
    "dnsv6": []
})")) << "DNS v6 addresses must be present in the v4 address list";

  // All fields must be provided.
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "All fields must be provided in JSON, missing version";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "All fields must be provided in JSON, missing netid";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
})")) << "All fields must be provided in JSON, missing mark";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "dnsv4": [],
    "dnsv6": []
})")) << "All fields must be provided in JSON, missing handle";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv6": []
})")) << "All fields must be provided in JSON, missing dnsv4";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/1",
                                R"({
    "version": "V1",
    "netid": 1,
    "mark": 56,
    "handle": 78,
    "dnsv4": []
})")) << "All fields must be provided in JSON, missing dnsv6";
}

TEST_F(NmfsTest, NmfsCannotCreateDir) {
  EXPECT_FALSE(files::CreateDirectory("/sys/fs/fuchsia_network_monitor_fs/2"));
}

TEST_F(NmfsTest, NmfsNetworkFileWriteSuccessNoAddresses) {
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/3",
                               R"({
    "version": "V1",
    "netid": 3,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
  })"))
      << "All JSON fields must be provided with proper formatting";
  // File output should be able to be rewritten to the file without formatting changes.
  std::string network_info;
  EXPECT_TRUE(files::ReadFileToString("/sys/fs/fuchsia_network_monitor_fs/3", &network_info))
      << "Network file should be readable";
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/3", network_info))
      << "Contents from read should be writeable to the same file";
}

TEST_F(NmfsTest, NmfsNetworkFileWriteSuccessV4Addresses) {
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/4",
                               R"({
    "version": "V1",
    "netid": 4,
    "mark": 56,
    "handle": 78,
    "dnsv4": ["192.0.2.0", "192.0.2.1"],
    "dnsv6": []
  })"))
      << "All JSON fields must be provided with proper formatting";
  // File output should be able to be rewritten to the file without formatting changes.
  std::string network_info;
  EXPECT_TRUE(files::ReadFileToString("/sys/fs/fuchsia_network_monitor_fs/4", &network_info))
      << "Network file should be readable";
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/4", network_info))
      << "Contents from read should be writeable to the same file";
}

TEST_F(NmfsTest, NmfsNetworkFileWriteSuccessV6Addresses) {
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/5",
                               R"({
    "version": "V1",
    "netid": 5,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": ["2001:db8::", "2001:db8::1"]
  })"))
      << "All JSON fields must be provided with proper formatting";
  // File output should be able to be rewritten to the file without formatting changes.
  std::string network_info;
  EXPECT_TRUE(files::ReadFileToString("/sys/fs/fuchsia_network_monitor_fs/5", &network_info))
      << "Network file should be readable";
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/5", network_info))
      << "Contents from read should be writeable to the same file";
}

TEST_F(NmfsTest, NmfsDefaultNetworkFile) {
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/default", "6"))
      << "The associated network file must be populated first prior to writing to the default file";

  std::string empty_default;
  EXPECT_FALSE(
      files::ReadFileToString("/sys/fs/fuchsia_network_monitor_fs/default", &empty_default))
      << "The default network id file must not be readable";

  // Create the network and set it as the default.
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/6",
                               R"({
    "version": "V1",
    "netid": 6,
    "mark": 56,
    "handle": 78,
    "dnsv4": [],
    "dnsv6": []
  })"))
      << "All JSON fields must be provided with proper formatting";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/default", "9999"))
      << "The default network id file must not accept a network id that does not exist";
  EXPECT_FALSE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/default", "null"))
      << "The default network id file must not accept a null network id";
  EXPECT_TRUE(files::WriteFile("/sys/fs/fuchsia_network_monitor_fs/default", "6"))
      << "The default network id file must accept the network id of a populated network";

  std::string populated_default;
  EXPECT_TRUE(
      files::ReadFileToString("/sys/fs/fuchsia_network_monitor_fs/default", &populated_default))
      << "The default network id file must be readable";
  EXPECT_EQ(populated_default, "6") << "The default network id must be the network id that was set";

  EXPECT_FALSE(files::DeletePath("/sys/fs/fuchsia_network_monitor_fs/6", /*recursive=*/false))
      << "The default network id must be unset before the network can be removed";
  EXPECT_TRUE(files::DeletePath("/sys/fs/fuchsia_network_monitor_fs/default", /*recursive=*/false))
      << "The default network id must be removable";
  EXPECT_TRUE(files::DeletePath("/sys/fs/fuchsia_network_monitor_fs/6", /*recursive=*/false))
      << "The default network id must be removable now that it is not the default";
}
