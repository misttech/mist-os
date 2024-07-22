// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>

#include <string_view>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace {

TEST(FakeDisplayCoordinatorConnector, ConnectToSvc) {
  zx::result<fidl::ClientEnd<fuchsia_hardware_display::Provider>> provider_result =
      component::Connect<fuchsia_hardware_display::Provider>();
  ASSERT_OK(provider_result);

  fidl::SyncClient<fuchsia_hardware_display::Provider> provider(std::move(provider_result).value());

  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Result open_coordinator_result =
      provider->OpenCoordinatorForPrimary(std::move(coordinator_server));
  EXPECT_TRUE(open_coordinator_result.is_ok())
      << "Failed to open coordinator: "
      << open_coordinator_result.error_value().FormatDescription();
}

TEST(FakeDisplayCoordinatorConnector, ConnectToDevfs) {
  static constexpr std::string_view kDevfsDirectory = "/dev/class/display-coordinator";

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> devfs_directory_result =
      component::OpenServiceRoot(kDevfsDirectory);
  ASSERT_OK(devfs_directory_result);
  fidl::ClientEnd<fuchsia_io::Directory> devfs_directory =
      std::move(devfs_directory_result).value();

  zx::result<std::string> service_filename_result =
      device_watcher::WatchDirectoryForItems<std::string>(
          devfs_directory, [&](std::string_view filename) { return std::string(filename); });
  ASSERT_OK(service_filename_result);

  std::string service_path = std::string(kDevfsDirectory) + "/" + service_filename_result.value();
  zx::result<fidl::ClientEnd<fuchsia_hardware_display::Provider>> provider_result =
      component::Connect<fuchsia_hardware_display::Provider>(service_path);
  ASSERT_OK(provider_result);

  fidl::SyncClient<fuchsia_hardware_display::Provider> provider(std::move(provider_result).value());

  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  fidl::Result open_coordinator_result =
      provider->OpenCoordinatorForPrimary(std::move(coordinator_server));
  EXPECT_TRUE(open_coordinator_result.is_ok())
      << "Failed to open coordinator: "
      << open_coordinator_result.error_value().FormatDescription();
}

}  // namespace
