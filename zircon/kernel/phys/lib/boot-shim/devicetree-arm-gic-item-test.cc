// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/zbitl/image.h>

#include "devicetree-test-fixture.h"

namespace {

using devicetree_test::LoadDtb;
using devicetree_test::LoadedDtb;

class ArmDevicetreeGicItemTest
    : public devicetree_test::TestMixin<devicetree_test::ArmDevicetreeTest,
                                        devicetree_test::SyntheticDevicetreeTest> {
 public:
  static void SetUpTestSuite() {
    Mixin::SetUpTestSuite();
    auto loaded_dtb = LoadDtb("arm_gic2_no_msi.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    gic2_no_msi_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    gic2_no_msi_ = std::nullopt;
    Mixin::TearDownTestSuite();
  }

  devicetree::Devicetree arm_gic2_no_msi() { return gic2_no_msi_->fdt(); }

 private:
  static std::optional<LoadedDtb> gic2_no_msi_;
};

std::optional<LoadedDtb> ArmDevicetreeGicItemTest::gic2_no_msi_ = std::nullopt;

TEST_F(ArmDevicetreeGicItemTest, ParseQemuGicV2WithMsi) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_arm_gic2();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  EXPECT_TRUE(shim.Init());
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->msi_frame_phys, 0x08020000);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicc_offset, 0x10000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_TRUE(dcfg->use_msi);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, GicV2NoMsi) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = arm_gic2_no_msi();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  shim.Init();
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->msi_frame_phys, 0);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicc_offset, 0x10000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->use_msi);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

// We dont support GicV3 with MSI yet, not reflected in the driver configuration.
TEST_F(ArmDevicetreeGicItemTest, ParseQemuGicV3) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_arm_gic3();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  EXPECT_TRUE(shim.Init());
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v3_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v3_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x08000000);
      EXPECT_EQ(dcfg->gicd_offset, 0x00000);
      EXPECT_EQ(dcfg->gicr_offset, 0xa0000);
      EXPECT_EQ(dcfg->gicr_stride, 0x20000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V3 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, ParseCrosvm) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = crosvm_arm();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  EXPECT_TRUE(shim.Init());
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v3_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v3_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0x3ffd0000);
      EXPECT_EQ(dcfg->gicd_offset, 0x20000);
      EXPECT_EQ(dcfg->gicr_offset, 0x00000);
      EXPECT_EQ(dcfg->gicr_stride, 0x20000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V3 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, KhadasVim3) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = khadas_vim3();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  shim.Init();
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2) {
      present = true;
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_arm_gic_v2_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_arm_gic_v2_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->mmio_phys, 0xffc01000);

      EXPECT_EQ(dcfg->gicd_offset, 0);
      EXPECT_EQ(dcfg->gicc_offset, 0x1000);
      EXPECT_EQ(dcfg->gich_offset, 0x3000);
      EXPECT_EQ(dcfg->gicv_offset, 0x5000);
      EXPECT_EQ(dcfg->ipi_base, 0x0);
      EXPECT_FALSE(dcfg->optional);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for GIC V2 missing.");
}

TEST_F(ArmDevicetreeGicItemTest, MissingNode) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = empty_fdt();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  // shim completes successfully even when nothing is matching.
  EXPECT_TRUE(shim.Init());
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  for (auto [header, payload] : image) {
    EXPECT_FALSE(header->type == ZBI_TYPE_KERNEL_DRIVER &&
                 (header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V3 ||
                  header->extra == ZBI_KERNEL_DRIVER_ARM_GIC_V2));
  }
  image.ignore_error();
}

}  // namespace
