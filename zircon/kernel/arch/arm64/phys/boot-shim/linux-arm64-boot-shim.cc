// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

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
#include <stdint.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <bind/fuchsia/platform/cpp/bind.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
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

// Specialized PoolMemConfigItem that allows attaching additional ranges. This is needed to
// provide the single range for peripherial [0, 1G].
//
// TODO(fxbug.dev/131475): Remove hack for hardcoded 1G peripherial range.
class QemuArm64PoolMemConfigItem : public boot_shim::PoolMemConfigItem {
 public:
  // Include the extra range.
  size_t size_bytes() const { return ItemSize(PayloadSize() + sizeof(zbi_mem_range_t)); }

  fit::result<DataZbi::Error> AppendItems(DataZbi& zbi) const {
    if (auto result = boot_shim::PoolMemConfigItem::AppendItems(zbi, 1); result.is_error()) {
      return result;
    }
    auto it = zbi.find(ZBI_TYPE_MEM_CONFIG);
    ZX_DEBUG_ASSERT(it != zbi.end());
    zbi.ignore_error();

    auto [header, payload] = *it;

    // The base class allocated enough space for our extra range.
    auto* extra_range = reinterpret_cast<zbi_mem_range_t*>(payload.data() + PayloadSize());
    *extra_range = {
        .paddr = 0,
        .length = 1 << 30,
        .type = ZBI_MEM_TYPE_PERIPHERAL,
    };

    return fit::ok();
  }
};

constexpr const char* kShimName = "linux-arm64-boot-shim";

// TODO(fxbug.dev/295031359): Once assembly generates this items, remove the hardcoded pair.
constexpr zbi_platform_id_t kQemuPlatformId = {
    .vid = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_QEMU,
    .pid = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_QEMU,
    .board_name = "qemu-arm64",
};

constexpr zbi_board_info_t kQemuBoardInfo = {
    .revision = 0x1,
};

}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  InitMemory(flat_devicetree_blob);
  MainSymbolize symbolize(kShimName);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  boot_shim::DevicetreeBootShim<
      boot_shim::UartItem<>, QemuArm64PoolMemConfigItem, boot_shim::ArmDevicetreePsciItem,
      boot_shim::ArmDevicetreeGicItem, boot_shim::DevicetreeDtbItem, PlatformIdItem, BoardInfoItem,
      boot_shim::ArmDevictreeCpuTopologyItem, boot_shim::ArmDevicetreeTimerItem>
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
  shim.Get<QemuArm64PoolMemConfigItem>().Init(Allocation::GetPool());
  shim.Get<boot_shim::DevicetreeDtbItem>().set_payload(
      {reinterpret_cast<const ktl::byte*>(gDevicetreeBoot.fdt.fdt().data()),
       gDevicetreeBoot.fdt.size_bytes()});
  shim.Get<PlatformIdItem>().set_payload(kQemuPlatformId);
  shim.Get<BoardInfoItem>().set_payload(kQemuBoardInfo);

  // Fill DevicetreeItems.
  shim.Init();

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
