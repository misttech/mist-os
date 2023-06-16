// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/path.h>
#include <lib/devicetree/testing/loaded-dtb.h>
#include <lib/fit/defer.h>
#include <lib/memalloc/range.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/item.h>
#include <lib/zbitl/storage-traits.h>

#include <array>
#include <memory>
#include <optional>
#include <type_traits>

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

class BootstrapChosenItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("qemu-arm-gic3.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    qemu_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("chosen.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    chosen_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    qemu_dtb_ = std::nullopt;
    chosen_dtb_ = std::nullopt;
  }

  devicetree::Devicetree qemu_dtb() { return qemu_dtb_->fdt(); }
  devicetree::Devicetree chosen_dtb() { return chosen_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> qemu_dtb_;
  static std::optional<LoadedDtb> chosen_dtb_;
};

std::optional<LoadedDtb> BootstrapChosenItemTest::qemu_dtb_ = std::nullopt;
std::optional<LoadedDtb> BootstrapChosenItemTest::chosen_dtb_ = std::nullopt;

struct ExpectedChosen {
  uintptr_t ramdisk_start;
  uintptr_t ramdisk_end;
  std::string_view cmdline;
  std::string_view uart_config_name;
  zbi_dcfg_simple_t uart_config;
  std::string_view uart_absolute_path;
};

using AllUartDrivers =
    std::variant<uart::null::Driver, uart::ns8250::LegacyDw8250Driver, uart::pl011::Driver>;

template <typename ChosenItemType>
void CheckChosenItem(const ChosenItemType& item, const ExpectedChosen& expected) {
  std::vector<std::unique_ptr<devicetree::Node>> nodes_in_path;
  size_t current = 0;
  while (current < expected.uart_absolute_path.length()) {
    size_t next = current;
    if (next = expected.uart_absolute_path.substr(current).find('/');
        next == std::string_view::npos) {
      next = expected.uart_absolute_path.length() - current;
    }
    nodes_in_path.push_back(
        std::make_unique<devicetree::Node>(expected.uart_absolute_path.substr(current, next)));
    current += next + 1;
  }

  devicetree::NodePath expected_uart_path;
  auto cleanup = fit::defer([&]() {
    while (!expected_uart_path.is_empty()) {
      expected_uart_path.pop_back();
    }
  });
  for (auto& node : nodes_in_path) {
    expected_uart_path.push_back(node.get());
  }

  // Cmdline Check.
  EXPECT_EQ(item.cmdline(), expected.cmdline);

  // Ramdisk captured correctly.
  auto ramdisk = item.zbi();
  uintptr_t ramdisk_start = reinterpret_cast<uintptr_t>(ramdisk.data());
  EXPECT_EQ(ramdisk_start, expected.ramdisk_start);
  EXPECT_EQ(ramdisk.size(), expected.ramdisk_end - expected.ramdisk_start);

  ASSERT_TRUE(item.stdout_path());
  EXPECT_EQ(devicetree::ComparePath(expected_uart_path, *item.stdout_path()),
            devicetree::CompareResult::kIsMatch);

  // Uart.
  std::optional<AllUartDrivers> uart = item.uart();

  ASSERT_TRUE(uart);

  std::visit(
      [&](auto& dcfg) {
        using config_t = std::decay_t<decltype(dcfg.config())>;
        if constexpr (std::is_same_v<config_t, zbi_dcfg_simple_t>) {
          EXPECT_EQ(dcfg.config_name(), expected.uart_config_name, "Actual name %s\n",
                    dcfg.config_name().data());
          EXPECT_EQ(dcfg.config().mmio_phys, expected.uart_config.mmio_phys);
          // The bootstrap phase does not decode interrupt.
          EXPECT_EQ(dcfg.config().irq, expected.uart_config.irq);
        } else {
          FAIL("Unexpected driver.");
        }
      },
      *uart);
}

TEST_F(BootstrapChosenItemTest, ParseChosen) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = chosen_dtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeBootstrapChosenNodeItem<AllUartDrivers>> shim(
      "test", fdt);

  shim.Init();

  auto& bootstrap_chosen_item =
      shim.Get<boot_shim::DevicetreeBootstrapChosenNodeItem<AllUartDrivers>>();
  CheckChosenItem(bootstrap_chosen_item,
                  {
                      .ramdisk_start = 0x48000000,
                      .ramdisk_end = 0x58000000,
                      .cmdline = "-foo=bar -bar=baz",
                      .uart_config_name = uart::pl011::Driver::config_name(),
                      .uart_config =
                          {
                              .mmio_phys = 0x9000000,
                              .irq = 0,
                          },
                      .uart_absolute_path = "/some-interrupt-controller/pl011uart@9000000",
                  });
}

TEST_F(BootstrapChosenItemTest, ParseQemuDtb) {
  constexpr std::string_view kQemuCmdline =
      "TERM=xterm-256color kernel.entropy-mixin=cd93b8955fc588b1bcde0d691a694b926d53faeca61c386635739b24df717363 kernel.halt-on-panic=true ";
  constexpr uint32_t kQemuRamdiskStart = 0x48000000;
  constexpr uint32_t kQemuRamdiskEnd = 0x499e8458;

  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_dtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeBootstrapChosenNodeItem<AllUartDrivers>> shim(
      "test", fdt);

  shim.Init();

  auto& bootstrap_chosen_item =
      shim.Get<boot_shim::DevicetreeBootstrapChosenNodeItem<AllUartDrivers>>();
  CheckChosenItem(bootstrap_chosen_item,
                  {
                      .ramdisk_start = kQemuRamdiskStart,
                      .ramdisk_end = kQemuRamdiskEnd,
                      .cmdline = kQemuCmdline,
                      .uart_config_name = uart::pl011::Driver::config_name(),
                      .uart_config =
                          {
                              .mmio_phys = uart::pl011::kQemuConfig.mmio_phys,
                              .irq = 0,
                          },
                      .uart_absolute_path = "/pl011@9000000",
                  });
}

}  // namespace
