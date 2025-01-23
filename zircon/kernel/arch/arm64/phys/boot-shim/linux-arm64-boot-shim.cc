// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/pool-mem-config.h>
#include <lib/boot-shim/uart.h>
#include <lib/fit/result.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/view.h>
#include <lib/zircon-internal/align.h>
#include <stddef.h>
#include <stdint.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/limits.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>
#include <ktl/align.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <ktl/type_traits.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/new.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

namespace {

constexpr const char* kShimName = "linux-arm64-boot-shim";

}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  // Explicitly provide `nullptr` address space, so the MMU is not set up.
  InitMemory(flat_devicetree_blob, nullptr);
  MainSymbolize symbolize(kShimName);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  boot_shim::DevicetreeBootShim<boot_shim::UartItem<>, boot_shim::PoolMemConfigItem,
                                boot_shim::NvramItem, boot_shim::ArmDevicetreePsciItem,
                                boot_shim::ArmDevicetreeGicItem, boot_shim::DevicetreeDtbItem,
                                boot_shim::ArmDevicetreeCpuTopologyItem,
                                boot_shim::ArmDevicetreeTimerItem>
      shim(kShimName, gDevicetreeBoot.fdt);
  shim.set_mmio_observer([&](boot_shim::DevicetreeMmioRange mmio_range) {
    auto& pool = Allocation::GetPool();
    memalloc::Range peripheral_range = {
        .addr = mmio_range.address,
        .size = mmio_range.size,
        .type = memalloc::Type::kPeripheral,
    };
    // This may reintroduce reserved ranges from the initial memory bootstrap as peripheral ranges,
    // since reserved ranges are no longer tracked and are represented as wholes in the memory. This
    // should be harmless, since the implications is that an uncached mapping will be created but
    // not touched.
    if (pool.MarkAsPeripheral(peripheral_range).is_error()) {
      printf("Failed to mark [%#" PRIx64 ", %#" PRIx64 "] as peripheral.\n", peripheral_range.addr,
             peripheral_range.end());
    }
  });
  shim.set_allocator([](size_t size, size_t align, fbl::AllocChecker& ac) -> void* {
    return new (ktl::align_val_t{align}, gPhysNew<memalloc::Type::kPhysScratch>, ac) uint8_t[size];
  });
  shim.set_cmdline(gDevicetreeBoot.cmdline);
  shim.Get<boot_shim::DevicetreeDtbItem>().set_payload(
      {reinterpret_cast<const ktl::byte*>(gDevicetreeBoot.fdt.fdt().data()),
       gDevicetreeBoot.fdt.size_bytes()});
  shim.Get<boot_shim::PoolMemConfigItem>().Init(Allocation::GetPool());
  if (gDevicetreeBoot.nvram) {
    shim.Get<boot_shim::NvramItem>().set_payload(*gDevicetreeBoot.nvram);
  }
  GetUartDriver().Visit([&shim]<typename KernelDriver>(const KernelDriver& driver) {
    // Mark the UART MMIO range as peripheral range.
    if (auto uart_mmio = GetUartMmioRange(driver, ZX_PAGE_SIZE)) {
      if (Allocation::GetPool().MarkAsPeripheral(*uart_mmio).is_error()) {
        printf("%s: Failed to mark [%#" PRIx64 ", %#" PRIx64 "] as peripheral.\n", shim.shim_name(),
               uart_mmio->addr, uart_mmio->end());
      }
    }
    // TODO(fxb/42084617): To be removed when driver state handed over switches do
    // `uart::all::Config` as we work to remove `uart()` accessor.
    shim.Get<boot_shim::UartItem<>>().Init(typename KernelDriver::uart_type(driver.config()));
  });

  // Fill DevicetreeItems.
  ZX_ASSERT(shim.Init());

  ArchSetUp(nullptr);

  // Finally we can boot into the kernel image.
  BootZbi::InputZbi zbi_view(gDevicetreeBoot.ramdisk);
  BootZbi boot;

  if (shim.Check("Not a bootable ZBI", boot.Init(zbi_view))) {
    // All MMIO Ranges have been observed by now.
    if (Allocation::GetPool().CoalescePeripherals(kAddressSpacePageSizeShifts).is_error()) {
      printf(
          "%s: WARNING Failed to inflate peripheral ranges, page allocation may be suboptimal.\n",
          shim.shim_name());
    }
    if (shim.Check("Failed to load ZBI", boot.Load(static_cast<uint32_t>(shim.size_bytes()))) &&
        shim.Check("Failed to append boot loader items to data ZBI",
                   shim.AppendItems(boot.DataZbi()))) {
      boot.Log();
      boot.Boot();
    }
  }
  __UNREACHABLE;
}
