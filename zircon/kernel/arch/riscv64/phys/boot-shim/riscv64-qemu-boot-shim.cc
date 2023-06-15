// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>
#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/boot-shim.h>
#include <lib/boot-shim/pool-mem-config.h>
#include <lib/boot-shim/test-serial-number.h>
#include <lib/boot-shim/uart.h>
#include <lib/devicetree/devicetree.h>
#include <lib/memalloc/pool.h>
#include <lib/stdcompat/span.h>
#include <lib/uart/qemu.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/zbi.h>

#include <bind/fuchsia/platform/cpp/bind.h>
#include <phys/allocation.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

namespace {

using DtbItem = boot_shim::SingleItem<ZBI_TYPE_DEVICETREE>;
using PlicItem = boot_shim::SingleOptionalItem<zbi_dcfg_riscv_plic_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                               ZBI_KERNEL_DRIVER_RISCV_PLIC>;
using TimerItem =
    boot_shim::SingleOptionalItem<zbi_dcfg_riscv_generic_timer_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                  ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER>;
using PlatformIdItem = boot_shim::SingleOptionalItem<zbi_platform_id_t, ZBI_TYPE_PLATFORM_ID>;
using BoardInfoItem = boot_shim::SingleOptionalItem<zbi_board_info_t, ZBI_TYPE_DRV_BOARD_INFO>;

using Shim = boot_shim::BootShim<boot_shim::PoolMemConfigItem,     //
                                 boot_shim::UartItem<>,            //
                                 boot_shim::TestSerialNumberItem,  //
                                 PlicItem, TimerItem, DtbItem, PlatformIdItem, BoardInfoItem>;

void InitMemory(const devicetree::Devicetree& dt, Shim::ByteView zbi) {
  // For now hard-wire a configuration matching what QEMU does with `-m 8192`.
  // Eventually this will come from memory nodes in the devicetree.
  constexpr uint64_t kRamStart = 0x80000000;
  memalloc::Range qemu_memory_ranges[] = {
      {
          // All of RAM.
          .addr = kRamStart,
          .size = uint64_t{8} << 30,  // 8 GiB
          .type = memalloc::Type::kFreeRam,
      },
      {
          // OpenSBI firmware uses this area.
          .addr = kRamStart,
          .size = uint64_t{512} << 10,  // 512 KiB
          .type = memalloc::Type::kReserved,
      },
  };

  const uint64_t phys_start = reinterpret_cast<uint64_t>(PHYS_LOAD_ADDRESS);
  const uint64_t phys_end = reinterpret_cast<uint64_t>(_end);
  memalloc::Range special_memory_ranges[] = {
      {
          .addr = phys_start,
          .size = phys_end - phys_start,
          .type = memalloc::Type::kPhysKernel,
      },
      {
          .addr = reinterpret_cast<uintptr_t>(dt.fdt().data()),
          .size = dt.size_bytes(),
          .type = memalloc::Type::kLegacyBootData,
      },
      {
          .addr = reinterpret_cast<uintptr_t>(zbi.data()),
          .size = zbi.size_bytes(),
          .type = memalloc::Type::kDataZbi,
      },
  };

  Allocation::Init(qemu_memory_ranges, special_memory_ranges);
}

ktl::optional<uintptr_t> UintptrFromProperty(devicetree::PropertyValue value) {
  if (auto result = value.AsUint64()) {
    return *result;
  }
  if (auto result = value.AsUint32()) {
    return *result;
  }
  return ktl::nullopt;
}

// Minimal dtb parsing just for cmdline and ramdisk (ZBI).
Shim::ByteView ParseDevicetree(Shim& shim, const devicetree::Devicetree& dt) {
  Shim::ByteView zbi;

  dt.Walk([&shim, &zbi](const auto& path, const auto& prop_decoder) -> bool {
    // The root node / is represented by the sequence {""}, not the empty path.
    ZX_DEBUG_ASSERT(!path.is_empty());
    auto it = ++path.begin();
    if (it == path.end() || *it != "chosen") {
      return true;
    }
    ktl::optional<ktl::string_view> cmdline;
    ktl::optional<uintptr_t> initrd_start, initrd_end;
    for (auto [name, value] : prop_decoder.properties()) {
      if (name == "bootargs") {
        cmdline = value.AsString();
      } else if (name == "linux,initrd-start") {
        initrd_start = UintptrFromProperty(value);
      } else if (name == "linux,initrd-end") {
        initrd_end = UintptrFromProperty(value);
      }
    }
    if (cmdline) {
      shim.set_cmdline(*cmdline);
    }
    if (initrd_start && initrd_end) {
      ZX_ASSERT_MSG(*initrd_end >= *initrd_start,
                    "linux,initrd-start %#" PRIxPTR " < linux,initrd-end %#" PRIxPTR, *initrd_start,
                    *initrd_end);
      zbi = {
          reinterpret_cast<ktl::byte*>(*initrd_start),
          *initrd_end - *initrd_start,
      };
    }
    return false;
  });

  return zbi;
}

}  // namespace

void PhysMain(void* ptr, arch::EarlyTicks boot_ticks) {
  InitStdout();

  ApplyRelocations();

  MainSymbolize symbolize("riscv64-qemu-boot-shim");

  Shim::ByteView dtb{static_cast<ktl::byte*>(ptr), SIZE_MAX};
  devicetree::Devicetree dt(dtb);

  static uart::qemu::KernelDriver<> uart;
  SetUartConsole(uart.uart());

  static BootOptions boot_opts;
  boot_opts.serial = uart.uart();
  gBootOptions = &boot_opts;

  Shim shim(symbolize.name());
  shim.set_build_id(symbolize.build_id());
  shim.set_info("QEMU -kernel argument");
  shim.Get<boot_shim::UartItem<>>().Init(GetUartDriver().uart());

  Shim::ByteView zbi = ParseDevicetree(shim, dt);
  ArchSetUp(const_cast<ktl::byte*>(zbi.data()));

  InitMemory(dt, zbi);
  memalloc::Pool& memory = Allocation::GetPool();
  memory.PrintMemoryRanges(symbolize.name());

  shim.Log(zbi);

  // The pool knows all the memory details, so populate the ZBI item that way.
  shim.Get<boot_shim::PoolMemConfigItem>().Init(memory);

  Shim::ByteView fdt{
      reinterpret_cast<const ktl::byte*>(dt.fdt().data()),
      dt.size_bytes(),
  };
  shim.Get<DtbItem>().set_payload(fdt);

  constexpr zbi_dcfg_riscv_plic_driver_t kPlicItem = {
      .mmio_phys = 0x0c00'0000,
      .num_irqs = 128,
  };
  shim.Get<PlicItem>().set_payload(kPlicItem);

  constexpr zbi_dcfg_riscv_generic_timer_driver_t kTimerItem = {
      .freq_hz = 10000000,
  };
  shim.Get<TimerItem>().set_payload(kTimerItem);
  shim.Get<PlatformIdItem>().set_payload(zbi_platform_id_t{
      .vid = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_QEMU,
      .pid = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_QEMU,
      .board_name = "qemu-riscv64",
  });
  shim.Get<BoardInfoItem>().set_payload(zbi_board_info_t{
      .revision = 0x1,
  });

  BootZbi boot;
  if (shim.Check("Not a bootable ZBI", boot.Init(Shim::InputZbi{zbi})) &&
      shim.Check("Failed to load ZBI", boot.Load(static_cast<uint32_t>(shim.size_bytes()))) &&
      shim.Check("Failed to append boot loader items to data ZBI",
                 shim.AppendItems(boot.DataZbi()))) {
    memory.PrintMemoryRanges(symbolize.name());
    boot.Log();
    boot.Boot();
  }

  abort();
}
