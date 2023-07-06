// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/testing/loaded-dtb.h>
#include <lib/fit/defer.h>
#include <lib/memalloc/range.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/item.h>

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
  EXPECT_EQ(*item.stdout_path(), expected_uart_path);

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

class MemoryItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("memory.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    memory_ldtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("reserved_memory.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    reserved_memory_ldtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("memory_reservations.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    memreserve_ldtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("memory_complex.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    complex_ldtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    memory_ldtb_ = std::nullopt;
    reserved_memory_ldtb_ = std::nullopt;
    memreserve_ldtb_ = std::nullopt;
    complex_ldtb_ = std::nullopt;
  }

  devicetree::Devicetree memory_ldtb() { return memory_ldtb_->fdt(); }
  devicetree::Devicetree reserved_memory_ldtb() { return reserved_memory_ldtb_->fdt(); }
  devicetree::Devicetree memreserve_ldtb() { return memreserve_ldtb_->fdt(); }
  devicetree::Devicetree complex_ldtb() { return complex_ldtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> memory_ldtb_;
  static std::optional<LoadedDtb> reserved_memory_ldtb_;
  static std::optional<LoadedDtb> memreserve_ldtb_;
  static std::optional<LoadedDtb> complex_ldtb_;
};

std::optional<LoadedDtb> MemoryItemTest::memory_ldtb_ = std::nullopt;
std::optional<LoadedDtb> MemoryItemTest::reserved_memory_ldtb_ = std::nullopt;
std::optional<LoadedDtb> MemoryItemTest::memreserve_ldtb_ = std::nullopt;
std::optional<LoadedDtb> MemoryItemTest::complex_ldtb_ = std::nullopt;

TEST_F(MemoryItemTest, ParseMemreserves) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = memreserve_ldtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem> shim("test", fdt);

  shim.Init();

  auto& mem_item = shim.Get<boot_shim::DevicetreeMemoryItem>();
  auto ranges = mem_item.memory_ranges();
  ASSERT_EQ(ranges.size(), 5);

  // Account for the devicetree in use.
  EXPECT_EQ(ranges[0].addr, reinterpret_cast<uintptr_t>(fdt.fdt().data()));
  EXPECT_EQ(ranges[0].size, fdt.size_bytes());
  EXPECT_EQ(ranges[0].type, memalloc::Type::kDevicetreeBlob);

  // Each memreserve in order.
  EXPECT_EQ(ranges[1].addr, 0x12340000);
  EXPECT_EQ(ranges[1].size, 0x2000);
  EXPECT_EQ(ranges[1].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[2].addr, 0x56780000);
  EXPECT_EQ(ranges[2].size, 0x3000);
  EXPECT_EQ(ranges[2].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[3].addr, 0x7fffffff12340000);
  EXPECT_EQ(ranges[3].size, 0x400000000);
  EXPECT_EQ(ranges[3].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[4].addr, 0x00ffffff56780000);
  EXPECT_EQ(ranges[4].size, 0x500000000);
  EXPECT_EQ(ranges[4].type, memalloc::Type::kReserved);
}

TEST_F(MemoryItemTest, ParseMemoryNodes) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = memory_ldtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem> shim("test", fdt);

  shim.Init();

  auto& mem_item = shim.Get<boot_shim::DevicetreeMemoryItem>();
  auto ranges = mem_item.memory_ranges();
  ASSERT_EQ(ranges.size(), 5);

  // Account for the devicetree in use.
  EXPECT_EQ(ranges[0].addr, reinterpret_cast<uintptr_t>(fdt.fdt().data()));
  EXPECT_EQ(ranges[0].size, fdt.size_bytes());
  EXPECT_EQ(ranges[0].type, memalloc::Type::kDevicetreeBlob);

  // Each memory nodes in order.
  EXPECT_EQ(ranges[1].addr, 0x40000000);
  EXPECT_EQ(ranges[1].size, 0x10000000);
  EXPECT_EQ(ranges[1].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[2].addr, 0x50000000);
  EXPECT_EQ(ranges[2].size, 0x20000000);
  EXPECT_EQ(ranges[2].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[3].addr, 0x60000000);
  EXPECT_EQ(ranges[3].size, 0x30000000);
  EXPECT_EQ(ranges[3].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[4].addr, 0x70000000);
  EXPECT_EQ(ranges[4].size, 0x40000000);
  EXPECT_EQ(ranges[4].type, memalloc::Type::kFreeRam);
}

TEST_F(MemoryItemTest, ParseReservedMemoryNodes) {
  std::array<std::byte, 256> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = reserved_memory_ldtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem> shim("test", fdt);

  shim.Init();

  auto& mem_item = shim.Get<boot_shim::DevicetreeMemoryItem>();
  auto ranges = mem_item.memory_ranges();
  ASSERT_EQ(ranges.size(), 3);

  // Account for the devicetree in use.
  EXPECT_EQ(ranges[0].addr, reinterpret_cast<uintptr_t>(fdt.fdt().data()));
  EXPECT_EQ(ranges[0].size, fdt.size_bytes());
  EXPECT_EQ(ranges[0].type, memalloc::Type::kDevicetreeBlob);

  // Each reserved memory nodes in order.
  EXPECT_EQ(ranges[1].addr, 0x78000000);
  EXPECT_EQ(ranges[1].size, 0x800000);
  EXPECT_EQ(ranges[1].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[2].addr, 0x76000000);
  EXPECT_EQ(ranges[2].size, 0x400000);
  EXPECT_EQ(ranges[2].type, memalloc::Type::kReserved);
}

TEST_F(MemoryItemTest, ParseAllAndAppend) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = complex_ldtb();
  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem> shim("test", fdt);

  shim.Init();

  auto& mem_item = shim.Get<boot_shim::DevicetreeMemoryItem>();
  auto ranges = mem_item.memory_ranges();
  ASSERT_EQ(ranges.size(), 11);

  // Account for the devicetree in use.
  EXPECT_EQ(ranges[0].addr, reinterpret_cast<uintptr_t>(fdt.fdt().data()));
  EXPECT_EQ(ranges[0].size, fdt.size_bytes());
  EXPECT_EQ(ranges[0].type, memalloc::Type::kDevicetreeBlob);

  // Each memreserve in order.
  EXPECT_EQ(ranges[1].addr, 0x12340000);
  EXPECT_EQ(ranges[1].size, 0x2000);
  EXPECT_EQ(ranges[1].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[2].addr, 0x56780000);
  EXPECT_EQ(ranges[2].size, 0x3000);
  EXPECT_EQ(ranges[2].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[3].addr, 0x7fffffff12340000);
  EXPECT_EQ(ranges[3].size, 0x400000000);
  EXPECT_EQ(ranges[3].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[4].addr, 0x00ffffff56780000);
  EXPECT_EQ(ranges[4].size, 0x500000000);
  EXPECT_EQ(ranges[4].type, memalloc::Type::kReserved);

  // Each memory nodes in order.
  EXPECT_EQ(ranges[5].addr, 0x40000000);
  EXPECT_EQ(ranges[5].size, 0x10000000);
  EXPECT_EQ(ranges[5].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[6].addr, 0x50000000);
  EXPECT_EQ(ranges[6].size, 0x20000000);
  EXPECT_EQ(ranges[6].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[7].addr, 0x60000000);
  EXPECT_EQ(ranges[7].size, 0x30000000);
  EXPECT_EQ(ranges[7].type, memalloc::Type::kFreeRam);

  EXPECT_EQ(ranges[8].addr, 0x70000000);
  EXPECT_EQ(ranges[8].size, 0x40000000);
  EXPECT_EQ(ranges[8].type, memalloc::Type::kFreeRam);

  // Each reserved memory nodes in order.
  EXPECT_EQ(ranges[9].addr, 0x78000000);
  EXPECT_EQ(ranges[9].size, 0x800000);
  EXPECT_EQ(ranges[9].type, memalloc::Type::kReserved);

  EXPECT_EQ(ranges[10].addr, 0x76000000);
  EXPECT_EQ(ranges[10].size, 0x400000);
  EXPECT_EQ(ranges[10].type, memalloc::Type::kReserved);

  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [h, p] : image) {
    if (h->type == ZBI_TYPE_MEM_CONFIG) {
      present = true;
      ASSERT_EQ(p.size(), zbitl::AlignedPayloadLength(11 * sizeof(zbi_mem_range_t)));
      cpp20::span<zbi_mem_range_t> zbi_ranges(reinterpret_cast<zbi_mem_range_t*>(p.data()), 11);
      // Each memreserve in order.
      EXPECT_EQ(zbi_ranges[0].paddr, reinterpret_cast<uintptr_t>(fdt.fdt().data()));
      EXPECT_EQ(zbi_ranges[0].length, fdt.size_bytes());
      EXPECT_EQ(zbi_ranges[0].type, ZBI_MEM_TYPE_RAM);

      EXPECT_EQ(zbi_ranges[1].paddr, 0x12340000);
      EXPECT_EQ(zbi_ranges[1].length, 0x2000);
      EXPECT_EQ(zbi_ranges[1].type, ZBI_MEM_TYPE_RESERVED);

      EXPECT_EQ(zbi_ranges[2].paddr, 0x56780000);
      EXPECT_EQ(zbi_ranges[2].length, 0x3000);
      EXPECT_EQ(zbi_ranges[2].type, ZBI_MEM_TYPE_RESERVED);

      EXPECT_EQ(zbi_ranges[3].paddr, 0x7fffffff12340000);
      EXPECT_EQ(zbi_ranges[3].length, 0x400000000);
      EXPECT_EQ(zbi_ranges[3].type, ZBI_MEM_TYPE_RESERVED);

      EXPECT_EQ(zbi_ranges[4].paddr, 0x00ffffff56780000);
      EXPECT_EQ(zbi_ranges[4].length, 0x500000000);
      EXPECT_EQ(zbi_ranges[4].type, ZBI_MEM_TYPE_RESERVED);

      // Each memory nodes in order.
      EXPECT_EQ(zbi_ranges[5].paddr, 0x40000000);
      EXPECT_EQ(zbi_ranges[5].length, 0x10000000);
      EXPECT_EQ(zbi_ranges[5].type, ZBI_MEM_TYPE_RAM);

      EXPECT_EQ(zbi_ranges[6].paddr, 0x50000000);
      EXPECT_EQ(zbi_ranges[6].length, 0x20000000);
      EXPECT_EQ(zbi_ranges[6].type, ZBI_MEM_TYPE_RAM);

      EXPECT_EQ(zbi_ranges[7].paddr, 0x60000000);
      EXPECT_EQ(zbi_ranges[7].length, 0x30000000);
      EXPECT_EQ(zbi_ranges[7].type, ZBI_MEM_TYPE_RAM);

      EXPECT_EQ(zbi_ranges[8].paddr, 0x70000000);
      EXPECT_EQ(zbi_ranges[8].length, 0x40000000);
      EXPECT_EQ(zbi_ranges[8].type, ZBI_MEM_TYPE_RAM);

      // Each reserved memory nodes in order.
      EXPECT_EQ(zbi_ranges[9].paddr, 0x78000000);
      EXPECT_EQ(zbi_ranges[9].length, 0x800000);
      EXPECT_EQ(zbi_ranges[9].type, ZBI_MEM_TYPE_RESERVED);

      EXPECT_EQ(zbi_ranges[10].paddr, 0x76000000);
      EXPECT_EQ(zbi_ranges[10].length, 0x400000);
      EXPECT_EQ(zbi_ranges[10].type, ZBI_MEM_TYPE_RESERVED);
    }
  }
  ASSERT_TRUE(present);
}

class RiscvDevicetreeTimerItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("cpus_riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    cpus_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("sifive-hifive-unmatched.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    sifive_hifive_unmatched_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("vision-five-2.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    vision_five_2_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("qemu-riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    qemu_riscv_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    cpus_dtb_ = std::nullopt;
    sifive_hifive_unmatched_dtb_ = std::nullopt;
    vision_five_2_dtb_ = std::nullopt;
    qemu_riscv_dtb_ = std::nullopt;
  }

  devicetree::Devicetree cpus_dtb() { return cpus_dtb_->fdt(); }
  devicetree::Devicetree sifive_hifive_unmatched() { return sifive_hifive_unmatched_dtb_->fdt(); }
  devicetree::Devicetree vision_five_2() { return vision_five_2_dtb_->fdt(); }
  devicetree::Devicetree qemu_riscv() { return qemu_riscv_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> cpus_dtb_;
  static std::optional<LoadedDtb> sifive_hifive_unmatched_dtb_;
  static std::optional<LoadedDtb> vision_five_2_dtb_;
  static std::optional<LoadedDtb> qemu_riscv_dtb_;
};

std::optional<LoadedDtb> RiscvDevicetreeTimerItemTest::cpus_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreeTimerItemTest::sifive_hifive_unmatched_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreeTimerItemTest::vision_five_2_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreeTimerItemTest::qemu_riscv_dtb_ = std::nullopt;

TEST_F(RiscvDevicetreeTimerItemTest, ParseTimerFromCpus) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = cpus_dtb();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x989680);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, Qemu) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_riscv();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x989680);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, VisionFive2) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = vision_five_2();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0x3D0900);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreeTimerItemTest, SifiveHifiveUnmatached) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = sifive_hifive_unmatched();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreeTimerItem> shim("test", fdt);
  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());

  bool present = false;
  auto clear_err = fit::defer([&]() { image.ignore_error(); });
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER &&
        header->extra == ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER) {
      EXPECT_GE(payload.size(), sizeof(zbi_dcfg_riscv_generic_timer_driver_t));
      auto* dcfg = reinterpret_cast<zbi_dcfg_riscv_generic_timer_driver_t*>(payload.data());
      EXPECT_EQ(dcfg->freq_hz, 0xf4240);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

class RiscvDevicetreePlicItemTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    auto loaded_dtb = LoadDtb("plic_riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    plic_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("sifive-hifive-unmatched.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    sifive_hifive_unmatched_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("vision-five-2.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    vision_five_2_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("qemu-riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    qemu_riscv_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    plic_dtb_ = std::nullopt;
    sifive_hifive_unmatched_dtb_ = std::nullopt;
    vision_five_2_dtb_ = std::nullopt;
    qemu_riscv_dtb_ = std::nullopt;
  }

  devicetree::Devicetree plic() { return plic_dtb_->fdt(); }
  devicetree::Devicetree sifive_hifive_unmatched() { return sifive_hifive_unmatched_dtb_->fdt(); }
  devicetree::Devicetree vision_five_2() { return vision_five_2_dtb_->fdt(); }
  devicetree::Devicetree qemu_riscv() { return qemu_riscv_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> plic_dtb_;
  static std::optional<LoadedDtb> sifive_hifive_unmatched_dtb_;
  static std::optional<LoadedDtb> vision_five_2_dtb_;
  static std::optional<LoadedDtb> qemu_riscv_dtb_;
};

std::optional<LoadedDtb> RiscvDevicetreePlicItemTest::plic_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreePlicItemTest::sifive_hifive_unmatched_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreePlicItemTest::vision_five_2_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevicetreePlicItemTest::qemu_riscv_dtb_ = std::nullopt;

TEST_F(RiscvDevicetreePlicItemTest, ParsePlic) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = plic();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreePlicItem> shim("test", fdt);

  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  auto clear_err = fit::defer([&]() { image.ignore_error(); });

  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_RISCV_PLIC) {
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_riscv_plic_driver_t));
      auto* plic_dcfg = reinterpret_cast<zbi_dcfg_riscv_plic_driver_t*>(payload.data());
      EXPECT_EQ(plic_dcfg->mmio_phys, 0xc000000);
      EXPECT_EQ(plic_dcfg->num_irqs, 0x60);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreePlicItemTest, Qemu) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_riscv();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreePlicItem> shim("test", fdt);

  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  auto clear_err = fit::defer([&]() { image.ignore_error(); });

  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_RISCV_PLIC) {
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_riscv_plic_driver_t));
      auto* plic_dcfg = reinterpret_cast<zbi_dcfg_riscv_plic_driver_t*>(payload.data());
      EXPECT_EQ(plic_dcfg->mmio_phys, 0xc000000);
      EXPECT_EQ(plic_dcfg->num_irqs, 0x60);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreePlicItemTest, VisionFive2) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = vision_five_2();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreePlicItem> shim("test", fdt);

  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  auto clear_err = fit::defer([&]() { image.ignore_error(); });

  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_RISCV_PLIC) {
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_riscv_plic_driver_t));
      auto* plic_dcfg = reinterpret_cast<zbi_dcfg_riscv_plic_driver_t*>(payload.data());
      EXPECT_EQ(plic_dcfg->mmio_phys, 0xc000000);
      EXPECT_EQ(plic_dcfg->num_irqs, 0x88);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevicetreePlicItemTest, HifiveSifiveUnmatched) {
  std::array<std::byte, 512> image_buffer;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = sifive_hifive_unmatched();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevicetreePlicItem> shim("test", fdt);

  shim.Init();
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  auto clear_err = fit::defer([&]() { image.ignore_error(); });

  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_KERNEL_DRIVER && header->extra == ZBI_KERNEL_DRIVER_RISCV_PLIC) {
      ASSERT_GE(payload.size(), sizeof(zbi_dcfg_riscv_plic_driver_t));
      auto* plic_dcfg = reinterpret_cast<zbi_dcfg_riscv_plic_driver_t*>(payload.data());
      EXPECT_EQ(plic_dcfg->mmio_phys, 0xc000000);
      EXPECT_EQ(plic_dcfg->num_irqs, 0x45);
      present = true;
    }
  }
  ASSERT_TRUE(present);
}

}  // namespace
