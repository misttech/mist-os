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
#include <lib/memalloc/range.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/view.h>
#include <lib/zircon-internal/align.h>
#include <stdint.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/limits.h>

#include <fbl/algorithm.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

namespace {

using PlatformIdItem = boot_shim::SingleOptionalItem<zbi_platform_id_t, ZBI_TYPE_PLATFORM_ID>;
using BoardInfoItem = boot_shim::SingleOptionalItem<zbi_board_info_t, ZBI_TYPE_DRV_BOARD_INFO>;

constexpr const char* kShimName = "linux-arm64-boot-shim";

// TODO(https://fxbug.dev/295031359): Once assembly generates this items, remove the hardcoded pair.
constexpr zbi_platform_id_t kQemuPlatformId = {
    .vid = 1,  // fuchsia.platform.BIND_PLATFORM_DEV_VID.QEMU
    .pid = 1,  // fuchsia.platform.BIND_PLATFORM_DEV_PID.QEMU
    .board_name = "qemu-arm64",
};

constexpr zbi_board_info_t kQemuBoardInfo = {
    .revision = 0x1,
};

}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  AddressSpace aspace;
  InitMemory(flat_devicetree_blob, &aspace);
  MainSymbolize symbolize(kShimName);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  boot_shim::DevicetreeBootShim<
      boot_shim::UartItem<>, boot_shim::PoolMemConfigItem, boot_shim::ArmDevicetreePsciItem,
      boot_shim::ArmDevicetreeGicItem, boot_shim::DevicetreeDtbItem, PlatformIdItem, BoardInfoItem,
      boot_shim::ArmDevictreeCpuTopologyItem, boot_shim::ArmDevicetreeTimerItem>
      shim(kShimName, gDevicetreeBoot.fdt);
  shim.set_mmio_observer([&](boot_shim::DevicetreeMmioRange mmio_range) {
    // This attempts to generate a peripheral ramge the covers as much as possible from the
    // non free ram ranges.
    auto& pool = Allocation::GetPool();
    memalloc::Range peripheral_range = {
        .addr = mmio_range.address,
        .size = mmio_range.size,
        .type = memalloc::Type::kPeripheral,
    };

    // Look for the range that comes right after `peripheral_range`.
    bool found = false;
    for (const auto range : pool) {
      if (range.addr >= peripheral_range.end()) {
        peripheral_range.size = range.addr - peripheral_range.addr;
        found = true;
        break;
      }
    }

    if (!found) {
      peripheral_range.size = fbl::round_up(peripheral_range.end(), 1ull << 30);
    }

    // Make sure generate ranges are always page aligned. The address is rounded to the containing
    // page, and the end is rounded up to the following page.
    uint64_t page_aligned_start = ZX_ROUNDDOWN(peripheral_range.addr, ZX_PAGE_SIZE);
    uint64_t page_aligned_end = ZX_PAGE_ALIGN(peripheral_range.end());
    peripheral_range.addr = page_aligned_start;
    peripheral_range.size = page_aligned_end - page_aligned_start;

    // This may reintroduce reserved ranges from the initial memory bootstrap as peripheral ranges,
    // since reserved ranges are no longer tracked and are represented as wholes in the memory. This
    // should be harmless, since the implications is that an uncached mapping will be created but
    // not touched.
    if (pool.MarkAsPeripheral(peripheral_range).is_error()) {
      printf("Failed to mark [%#" PRIx64 ", %#" PRIx64 "] as peripheral.\n", peripheral_range.addr,
             peripheral_range.end());
    }
  });
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
  shim.Get<boot_shim::DevicetreeDtbItem>().set_payload(
      {reinterpret_cast<const ktl::byte*>(gDevicetreeBoot.fdt.fdt().data()),
       gDevicetreeBoot.fdt.size_bytes()});
  shim.Get<PlatformIdItem>().set_payload(kQemuPlatformId);
  shim.Get<BoardInfoItem>().set_payload(kQemuBoardInfo);

  // Fill DevicetreeItems.
  ZX_ASSERT(shim.Init());

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
