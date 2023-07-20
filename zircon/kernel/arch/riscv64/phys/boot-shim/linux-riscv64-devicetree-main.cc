// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>
#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/boot-shim/pool-mem-config.h>
#include <lib/boot-shim/uart.h>
#include <lib/zbitl/view.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <ktl/string_view.h>
#include <phys/allocation.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

namespace {

constexpr const char* kShimName = "devicetree-boot-shim";
}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  InitMemory(flat_devicetree_blob);

  MainSymbolize symbolize(kShimName);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  // The list here is architecture dependant, and should be factored out eventually.
  static boot_shim::DevicetreeBootShim<
      boot_shim::UartItem<>, boot_shim::PoolMemConfigItem, boot_shim::RiscvDevicetreePlicItem,
      boot_shim::RiscvDevicetreeTimerItem, boot_shim::RiscvDevictreeCpuTopologyItem>
      shim(kShimName, gDevicetreeBoot.fdt);
  shim.set_allocator([](size_t size, size_t align) -> void* {
    if (auto alloc = Allocation::GetPool().Allocate(memalloc::Type::kPhysScratch, size, align);
        alloc.is_ok()) {
      return reinterpret_cast<void*>(*alloc);
    }
    return nullptr;
  });
  shim.set_cmdline(gDevicetreeBoot.cmdline);
  shim.Get<boot_shim::UartItem<>>().Init(GetUartDriver().uart());
  shim.Get<boot_shim::PoolMemConfigItem>().Init(Allocation::GetPool());

  // Fill DevicetreeItems.
  shim.Init();

  // Use the generated zbi to do some setup.
  ArchSetUp(nullptr);

  // Finally we can boot into the kernel image.
  BootZbi::InputZbi zbi_view(gDevicetreeBoot.ramdisk);
  BootZbi boot;

  if (shim.Check("Not a bootable ZBI", boot.Init(zbi_view)) &&
      shim.Check("Failed to load ZBI", boot.Load(static_cast<uint32_t>(shim.size_bytes()))) &&
      shim.Check("Failed to append boot loader items to data ZBI",
                 shim.AppendItems(boot.DataZbi()))) {
    boot.Log();
    boot.Boot();
  }
  __UNREACHABLE;
}
