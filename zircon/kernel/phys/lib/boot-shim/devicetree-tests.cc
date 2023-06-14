// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/testing/loaded-dtb.h>
#include <lib/zbi-format/driver-config.h>

#include <array>
#include <memory>
#include <optional>

#include <zxtest/zxtest.h>

namespace {

using devicetree::testing::LoadDtb;
using devicetree::testing::LoadedDtb;

class ArmDevicetreePsciItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("psci-hvc.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    psci_hvc_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("psci-smc.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    psci_smc_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    psci_hvc_dtb_ = std::nullopt;
    psci_smc_dtb_ = std::nullopt;
  }

  devicetree::Devicetree psci_hvc() { return psci_hvc_dtb_->fdt(); }
  devicetree::Devicetree psci_smc() { return psci_smc_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> psci_hvc_dtb_;
  static std::optional<LoadedDtb> psci_smc_dtb_;
};

std::optional<LoadedDtb> ArmDevicetreePsciItemTest::psci_hvc_dtb_ = std::nullopt;
std::optional<LoadedDtb> ArmDevicetreePsciItemTest::psci_smc_dtb_ = std::nullopt;

TEST_F(ArmDevicetreePsciItemTest, ParseSmc) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = psci_smc();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreePsciItem> shim("test", fdt);

  shim.Init();
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_PSCI) {
      present = true;
      ASSERT_EQ(payload.size(), sizeof(zbi_dcfg_arm_psci_driver_t));
      const auto* dcfg = reinterpret_cast<const zbi_dcfg_arm_psci_driver_t*>(payload.data());
      EXPECT_FALSE(dcfg->use_hvc);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for PSCI missing.");
}

TEST_F(ArmDevicetreePsciItemTest, ParseHvc) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = psci_hvc();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreePsciItem> shim("test", fdt);

  shim.Init();
  EXPECT_TRUE(shim.AppendItems(image).is_ok());

  // Look for a gic 2 driver.
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_ARM_PSCI) {
      present = true;
      ASSERT_EQ(payload.size(), sizeof(zbi_dcfg_arm_psci_driver_t));
      const auto* dcfg = reinterpret_cast<const zbi_dcfg_arm_psci_driver_t*>(payload.data());
      EXPECT_TRUE(dcfg->use_hvc);
      break;
    }
  }
  image.ignore_error();
  ASSERT_TRUE(present, "ZBI Driver for PSCI missing.");
}

class ArmDevicetreeGicItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("qemu-arm-gic2.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    gic2_ldtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("qemu-arm-gic3.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    gic3_ldtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    gic2_ldtb_ = std::nullopt;
    gic3_ldtb_ = std::nullopt;
  }

  devicetree::Devicetree qemu_gic2_msi() { return gic2_ldtb_->fdt(); }

  devicetree::Devicetree qemu_gic3_msi() { return gic3_ldtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> gic2_ldtb_;
  static std::optional<LoadedDtb> gic3_ldtb_;
};

std::optional<LoadedDtb> ArmDevicetreeGicItemTest::gic2_ldtb_ = std::nullopt;
std::optional<LoadedDtb> ArmDevicetreeGicItemTest::gic3_ldtb_ = std::nullopt;

TEST_F(ArmDevicetreeGicItemTest, ParseGicV2WithMsi) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_gic2_msi();
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

// We dont support GicV3 with MSI yet, not reflected in the driver configuration.
TEST_F(ArmDevicetreeGicItemTest, ParseGicV3) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_gic3_msi();
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreeGicItem> shim("test", fdt);

  shim.Init();
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

}  // namespace
