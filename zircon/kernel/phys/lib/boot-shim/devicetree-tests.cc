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

}  // namespace
