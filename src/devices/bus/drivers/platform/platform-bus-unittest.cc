// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/devices/bus/drivers/platform/platform-bus.h"

#include <lib/fake-bti/bti.h>
#include <zircon/status.h>

#include <zxtest/zxtest.h>

#include "src/devices/bus/drivers/platform/node-util.h"

namespace {

static size_t g_bti_created = 0;

TEST(PlatformBusTest, IommuGetBti) {
  g_bti_created = 0;
  platform_bus::PlatformBus pbus(nullptr, zx::channel());
  EXPECT_EQ(g_bti_created, 0);
  zx::bti bti;
  ASSERT_OK(pbus.IommuGetBti(0, 0, &bti));
  EXPECT_EQ(g_bti_created, 1);
  ASSERT_OK(pbus.IommuGetBti(0, 0, &bti));
  EXPECT_EQ(g_bti_created, 1);
  ASSERT_OK(pbus.IommuGetBti(0, 1, &bti));
  EXPECT_EQ(g_bti_created, 2);
}

TEST(PlatformBusTest, GetMmioIndex) {
  const std::vector<fuchsia_hardware_platform_bus::Mmio> mmios{
      {{
          .base = 1,
          .length = 2,
          .name = "first",
      }},
      {{
          .base = 3,
          .length = 4,
          .name = "second",
      }},
  };

  fuchsia_hardware_platform_bus::Node node = {};
  node.mmio() = mmios;

  {
    auto result = platform_bus::GetMmioIndex(node, "first");
    ASSERT_TRUE(result.has_value());
    ASSERT_EQ(result.value(), 0);
  }
  {
    auto result = platform_bus::GetMmioIndex(node, "second");
    ASSERT_TRUE(result.has_value());
    ASSERT_EQ(result.value(), 1);
  }
  {
    auto result = platform_bus::GetMmioIndex(node, "none");
    ASSERT_FALSE(result.has_value());
  }
}

TEST(PlatformBusTest, GetMmioIndexNoMmios) {
  fuchsia_hardware_platform_bus::Node node = {};
  {
    auto result = platform_bus::GetMmioIndex(node, "none");
    ASSERT_FALSE(result.has_value());
  }
}

}  // namespace

__EXPORT
zx_status_t zx_bti_create(zx_handle_t handle, uint32_t options, uint64_t bti_id, zx_handle_t* out) {
  g_bti_created++;
  return fake_bti_create(out);
}
