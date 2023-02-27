// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/manifest_parser.h"

#include <zxtest/zxtest.h>

#include "src/devices/bin/driver_manager/driver.h"

TEST(ManifestParserTest, FuchsiaUrlToPath) {
  auto result = GetPathFromUrl("fuchsia-pkg://fuchsia.com/my-package#driver/my-driver.so");
  ASSERT_EQ(result.status_value(), ZX_OK);
  ASSERT_EQ(result.value(), "/pkgfs/packages/my-package/0/driver/my-driver.so");
}

TEST(ManifestParserTest, BootUrlToPath) {
  auto result = GetPathFromUrl("fuchsia-boot:///#driver/my-driver.so");
  ASSERT_EQ(result.status_value(), ZX_OK);
  ASSERT_EQ(result.value(), "/boot/driver/my-driver.so");
}

TEST(ManifestParserTest, FuchsiaUrlToBasePath) {
  auto result = GetBasePathFromUrl("fuchsia-pkg://fuchsia.com/my-package#driver/my-driver.so");
  ASSERT_EQ(result.status_value(), ZX_OK);
  ASSERT_EQ(result.value(), "/pkgfs/packages/my-package/0");
}

TEST(ManifestParserTest, BootUrlToBasePath) {
  auto result = GetBasePathFromUrl("fuchsia-boot:///#driver/my-driver.so");
  ASSERT_EQ(result.status_value(), ZX_OK);
  ASSERT_EQ(result.value(), "/boot");
}

TEST(ManifestParserTest, ParseComponentManifest) {
  constexpr const char kDriverManifestPath[] = "/pkg/meta/manifest-test.cm";
  zx::result manifest_vmo = load_manifest_vmo(kDriverManifestPath);
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

TEST(ManifestParserTest, ParseComponentManifest_InvalidVmo) {
  zx::result manifest = ParseComponentManifest(zx::vmo{});
  ASSERT_NOT_OK(manifest.status_value());
}

TEST(ManifestParserTest, ParseComponentManifest_MissingDriver) {
  constexpr const char kDriverManifestPath[] = "/pkg/meta/manifest-missing-driver.cm";
  zx::result manifest_vmo = load_manifest_vmo(kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  ASSERT_NOT_OK(manifest.status_value());
}

TEST(ManifestParserTest, ParseComponentManifest_NoServices) {
  constexpr const char kDriverManifestPath[] = "/pkg/meta/manifest-no-services.cm";
  zx::result manifest_vmo = load_manifest_vmo(kDriverManifestPath);
  ASSERT_OK(manifest_vmo.status_value());

  zx::result manifest = ParseComponentManifest(std::move(manifest_vmo.value()));
  std::vector<std::string> kExpectedUses{};
  EXPECT_EQ(manifest->service_uses, kExpectedUses);
}
