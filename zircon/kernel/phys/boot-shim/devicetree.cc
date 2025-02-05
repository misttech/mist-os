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
#include <variant>

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

}  // namespace

void InitMemory(void* dtb, AddressSpace* aspace) {
  static std::array<memalloc::Range, kDevicetreeMaxMemoryRanges> range_storage;

  devicetree::ByteView fdt_blob(static_cast<const uint8_t*>(dtb),
                                std::numeric_limits<uintptr_t>::max());
  devicetree::Devicetree fdt(fdt_blob);

  boot_shim::DevicetreeMemoryMatcher memory("init-memory", stdout, range_storage);
  boot_shim::DevicetreeChosenNodeMatcher<> chosen("init-memory", stdout);
  ZX_ASSERT(devicetree::Match(fdt, chosen, memory));

  //
  // The following 'special' memory ranges are those that we already know are
  // populated.
  //
  uint64_t phys_start = reinterpret_cast<uint64_t>(PHYS_LOAD_ADDRESS);
  uint64_t phys_end = reinterpret_cast<uint64_t>(_end);
  ktl::array<memalloc::Range, 4> special_range_storage = {
      memalloc::Range{
          .addr = phys_start,
          .size = phys_end - phys_start,
          .type = memalloc::Type::kPhysKernel,
      },
      {
          .addr = reinterpret_cast<uintptr_t>(fdt.fdt().data()),
          .size = fdt.size_bytes(),
          .type = memalloc::Type::kDevicetreeBlob,
      },
  };
  cpp20::span<memalloc::Range> special_ranges = cpp20::span{special_range_storage}.subspan(0, 2);

  if (!chosen.zbi().empty()) {
    special_range_storage[special_ranges.size()] = memalloc::Range{
        .addr = reinterpret_cast<uintptr_t>(chosen.zbi().data()),
        .size = chosen.zbi().size(),
        .type = memalloc::Type::kDataZbi,
    };
    special_ranges = cpp20::span(special_range_storage).subspan(0, special_ranges.size() + 1);
  }

  if (memory.nvram()) {
    special_range_storage[special_ranges.size()] = {
        .addr = memory.nvram()->base,
        .size = memory.nvram()->length,
        .type = memalloc::Type::kNvram,
    };
    special_ranges = cpp20::span(special_range_storage).subspan(0, special_ranges.size() + 1);
  }

  // The matching phase above recorded all of the memory ranges encoded within
  // the devicetree tree structure, leaving the memory reservations. Since
  // bootloaders may sometimes generate spurious memory reservations for things
  // like the devicetree blob and ramdisk, we take care to exclude those ranges
  // from the translation to RESERVED ranges. This is handled by
  // ForEachDevicetreeMemoryReservation below.
  cpp20::span<memalloc::Range> ranges;
  {
    // ForEachDevicetreeMemoryReservation requires that the 'exclusions' be
    // non-overlapping and sorted. The special ranges are surely
    // non-overlapping.
    std::sort(special_ranges.begin(), special_ranges.end(),
              [](auto a, auto b) { return (a.addr < b.addr); });
    size_t written = memory.ranges().size();
    bool recorded = boot_shim::ForEachDevicetreeMemoryReservation(
        fdt, /*exclusions=*/special_ranges, [&written](devicetree::MemoryReservation res) {
          if (written >= range_storage.size()) {
            return false;
          }
          range_storage[written++] = memalloc::Range{
              .addr = res.start,
              .size = res.size,
              .type = memalloc::Type::kReserved,
          };
          return true;
        });
    ZX_ASSERT_MSG(recorded, "Insufficient space to record devicetree memory reservations");
    ranges = cpp20::span{range_storage}.subspan(0, written);
  }

  // This instance of |BootOptions| is not meant to be wired anywhere, its sole purpose is to select
  // the proper uart from the cmdline if its present.
  static BootOptions boot_options;

  // If the chosen matcher did not find a serial console setting, keep whatever
  // current setting was in place before calling InitMemory. That's often the
  // null driver, but could be something else.
  boot_options.serial = chosen.TakeUart().value_or(ktl::move(GetUartDriver()).TakeUart());
  SetBootOptionsWithoutEntropy(boot_options, {}, chosen.cmdline().value_or(""));
  SetUartConsole(boot_options.serial);

  Allocation::Init(ranges, special_ranges);
  if (aspace) {
    ArchSetUpAddressSpace(*aspace);
  }
  Allocation::GetPool().PrintMemoryRanges(ProgramName());

  gDevicetreeBoot = {
      .cmdline = chosen.cmdline().value_or(""),
      .ramdisk = chosen.zbi(),
      .fdt = fdt,
      .nvram = memory.nvram(),
  };
}
