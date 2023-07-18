// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/manifest_parser.h"

#include <fcntl.h>
#include <lib/fdio/fd.h>

#include <zxtest/zxtest.h>

#include "src/devices/bin/driver_manager/v1/driver.h"

namespace {

zx::result<fidl::WireSyncClient<fuchsia_io::Directory>> GetCurrentPackageDirectory() {
  int fd;
  if ((fd = open("/pkg", O_RDONLY, O_DIRECTORY)) < 0) {
    LOGF(ERROR, "Failed to open /pkg");
    return zx::error(ZX_ERR_INTERNAL);
  }

  fidl::ClientEnd<fuchsia_io::Directory> client_end;
  if (zx_status_t status = fdio_fd_transfer(fd, client_end.channel().reset_and_get_address());
      status != ZX_OK) {
    LOGF(ERROR, "Failed to transfer fd from /pkg");
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(fidl::WireSyncClient<fuchsia_io::Directory>{std::move(client_end)});
}

}  // namespace

TEST(ManifestParserTest, BootUrlToBasePath) {
  auto result = GetBasePathFromUrl("fuchsia-boot:///#driver/my-driver.so");
  ASSERT_EQ(result.status_value(), ZX_OK);
  ASSERT_EQ(result.value(), "/boot");
}

TEST(ManifestParserTest, ParseComponentManifest) {
  constexpr const char kDriverManifestPath[] = "meta/manifest-test.cm";
  zx::result pkg_dir_result = GetCurrentPackageDirectory();
  ASSERT_OK(pkg_dir_result.status_value());

  zx::result manifest_vmo = load_manifest_vmo(pkg_dir_result.value(), kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  ASSERT_OK(manifest.status_value());
  EXPECT_EQ(manifest->driver_url, "#driver/manifest-test.so");
  std::vector<std::string> kExpectedUses = {
      "/svc/fuchsia.hardware.acpi.Service",
      "/svc/fuchsia.hardware.pci.Service",
  };
  EXPECT_EQ(manifest->service_uses, kExpectedUses);
}

TEST(ManifestParserTest, ParseComponentManifest_SchedulerRole) {
  constexpr const char kDriverManifestPath[] = "meta/manifest-with-scheduler-role.cm";
  zx::result pkg_dir_result = GetCurrentPackageDirectory();
  ASSERT_OK(pkg_dir_result.status_value());

  zx::result manifest_vmo = load_manifest_vmo(pkg_dir_result.value(), kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  ASSERT_OK(manifest.status_value());
  EXPECT_EQ(manifest->default_dispatcher_scheduler_role, "fuchsia.test-role:ok");
}

TEST(ManifestParserTest, ParseComponentManifest_InvalidVmo) {
  zx::result manifest = ParseComponentManifest(zx::vmo{});
  ASSERT_NOT_OK(manifest.status_value());
}

TEST(ManifestParserTest, ParseComponentManifest_MissingDriver) {
  constexpr const char kDriverManifestPath[] = "meta/manifest-missing-driver.cm";
  zx::result pkg_dir_result = GetCurrentPackageDirectory();
  ASSERT_OK(pkg_dir_result.status_value());

  zx::result manifest_vmo = load_manifest_vmo(pkg_dir_result.value(), kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  ASSERT_NOT_OK(manifest.status_value());
}

TEST(ManifestParserTest, ParseComponentManifest_NoServices) {
  constexpr const char kDriverManifestPath[] = "meta/manifest-no-services.cm";
  zx::result pkg_dir_result = GetCurrentPackageDirectory();
  ASSERT_OK(pkg_dir_result.status_value());

  zx::result manifest_vmo = load_manifest_vmo(pkg_dir_result.value(), kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  std::vector<std::string> kExpectedUses{};
  EXPECT_EQ(manifest->service_uses, kExpectedUses);
}
