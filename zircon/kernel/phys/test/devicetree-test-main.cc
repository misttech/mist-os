// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>
#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/uart/all.h>
#include <lib/uart/null.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/storage-traits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/assert.h>

#include <array>

#include <fbl/alloc_checker.h>
#include <ktl/array.h>
#include <ktl/limits.h>
#include <ktl/span.h>
#include <ktl/type_traits.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/boot-options.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/uart.h>

#include "test-main.h"

namespace {

cpp20::span<memalloc::Range> gMemoryRanges;
std::array<memalloc::Range, kDevicetreeMaxMemoryRanges> gMemoryStorage;
zbitl::ByteView gZbi;
void* gDevicetreeBlob = nullptr;

}  // namespace

void InitMemory(void* dtb) {
  ZX_ASSERT(dtb == gDevicetreeBlob);
  uint64_t phys_start = reinterpret_cast<uint64_t>(PHYS_LOAD_ADDRESS);
  uint64_t phys_end = reinterpret_cast<uint64_t>(_end);

  ktl::array<memalloc::Range, 2> special_ranges = {
      memalloc::Range{
          .addr = phys_start,
          .size = phys_end - phys_start,
          .type = memalloc::Type::kPhysKernel,
      },
  };
  cpp20::span<memalloc::Range> special_ranges_view(special_ranges.data(), 1);

  if (!gZbi.empty()) {
    special_ranges[1] = memalloc::Range{
        .addr = reinterpret_cast<uint64_t>(gZbi.data()),
        .size = gZbi.size(),
        .type = memalloc::Type::kDataZbi,
    };
    special_ranges_view = special_ranges;
  }

  Allocation::Init(gMemoryRanges, special_ranges_view);

  ArchSetUpAddressSpaceEarly();

  if (gBootOptions->phys_verbose) {
    Allocation::GetPool().PrintMemoryRanges(ProgramName());
  }
}

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  devicetree::ByteView fdt_blob(static_cast<const uint8_t*>(flat_devicetree_blob),
                                std::numeric_limits<uintptr_t>::max());

  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem,
                                boot_shim::DevicetreeBootstrapChosenNodeItem<>>
      shim("devicetree-test-main", devicetree::Devicetree(fdt_blob));

  auto& memory_item = shim.Get<boot_shim::DevicetreeMemoryItem>();
  memory_item.InitStorage(gMemoryStorage);
  shim.Init();

  auto& chosen_item = shim.Get<boot_shim::DevicetreeBootstrapChosenNodeItem<>>();

  static BootOptions boot_opts;
  gBootOptions = &boot_opts;
  boot_opts.serial = chosen_item.uart().uart();
  SetBootOptions(boot_opts, chosen_item.zbi(), chosen_item.cmdline().value_or(""));
  SetUartConsole(boot_opts.serial);

  gMemoryRanges =
      cpp20::span<memalloc::Range>(const_cast<memalloc::Range*>(memory_item.memory_ranges().data()),
                                   memory_item.memory_ranges().size());
  gDevicetreeBlob = flat_devicetree_blob;
  gBootOptions = &boot_opts;
  gZbi = chosen_item.zbi();

  ArchSetUp(nullptr);

  // Early boot may have filled the screen with logs. Add a newline to
  // terminate any previous line, and another newline to leave a blank.
  printf("\n\n");

  // Run the test.
  int status = TestMain(flat_devicetree_blob, ticks);
  if (status == 0) {
    printf("\n*** Test succeeded ***\n%s\n\n", BOOT_TEST_SUCCESS_STRING);
  } else {
    printf("\n*** Test FAILED: status %d ***\n\n", status);
  }

  // No way to shut down.
  abort();
}
