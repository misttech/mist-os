// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/boot-shim/devicetree.h"

#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/memalloc/range.h>
#include <zircon/assert.h>

#include <array>

#include <ktl/array.h>
#include <ktl/type_traits.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/boot-options.h>
#include <phys/main.h>
#include <phys/uart.h>

DevicetreeBoot gDevicetreeBoot;

namespace {

// Other platforms such as Linux provide a few of preallocated buffers for storing memory ranges,
// |kMaxRanges| is a big enough upperbound for the combined number of ranges provided by such
// buffers.
//
// This represents a recommended number of entries to be allocated for storing real world memory
// ranges.
constexpr size_t kDevicetreeMaxMemoryRanges = 512;

void DevicetreeInitMemory(zbitl::ByteView zbi, const boot_shim::DevicetreeMemoryMatcher& memory) {
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

  if (!zbi.empty()) {
    special_ranges[1] = memalloc::Range{
        .addr = reinterpret_cast<uint64_t>(zbi.data()),
        .size = zbi.size(),
        .type = memalloc::Type::kDataZbi,
    };
    special_ranges_view = special_ranges;
  }

  auto ranges = memory.memory_ranges();
  auto memory_ranges =
      cpp20::span<memalloc::Range>(const_cast<memalloc::Range*>(ranges.data()), ranges.size());
  Allocation::Init(memory_ranges, special_ranges_view);
  ArchSetUpAddressSpaceEarly();

  Allocation::GetPool().PrintMemoryRanges(ProgramName());
}

}  // namespace

void InitMemory(void* dtb) {
  static std::array<memalloc::Range, kDevicetreeMaxMemoryRanges> memory_ranges;
  devicetree::ByteView fdt_blob(static_cast<const uint8_t*>(dtb),
                                std::numeric_limits<uintptr_t>::max());
  devicetree::Devicetree fdt(fdt_blob);

  boot_shim::DevicetreeMemoryMatcher memory("init-memory", stdout, memory_ranges);
  boot_shim::DevicetreeChosenNodeMatcher<> chosen("init-memory", stdout);

  memory.AppendAdditionalRanges(fdt);
  ZX_ASSERT(devicetree::Match(fdt, chosen, memory));

  // This instance of |BootOptions| is not meant to be wired anywhere, its sole purpose is to select
  // the proper uart from the cmdline if its present.
  static BootOptions boot_options;
  boot_options.serial = chosen.uart().uart();
  SetBootOptions(boot_options, {}, chosen.cmdline().value_or(""));
  SetUartConsole(boot_options.serial);
  DevicetreeInitMemory(chosen.zbi(), memory);

  gDevicetreeBoot = {
      .cmdline = chosen.cmdline().value_or(""),
      .ramdisk = chosen.zbi(),
      .fdt = fdt,
  };
}
