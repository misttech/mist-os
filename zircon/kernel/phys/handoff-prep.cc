// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "handoff-prep.h"

#include <ctype.h>
#include <lib/boot-options/boot-options.h>
#include <lib/instrumentation/debugdata.h>
#include <lib/llvm-profdata/llvm-profdata.h>
#include <lib/memalloc/pool-mem-config.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/trivial-allocator/new.h>
#include <lib/zbitl/error-stdio.h>
#include <stdio.h>
#include <string-file.h>
#include <zircon/assert.h>

#include <ktl/algorithm.h>
#include <ktl/tuple.h>
#include <phys/allocation.h>
#include <phys/arch/arch-handoff.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/main.h>
#include <phys/new.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include "log.h"
#include "physboot.h"

#include <ktl/enforce.h>

namespace {

// Carve out some physical pages requested for testing before handing off.
void FindTestRamReservation(RamReservation& ram) {
  ZX_ASSERT_MSG(!ram.paddr, "Must use kernel.test.ram.reserve=SIZE without ,ADDRESS!");

  memalloc::Pool& pool = Allocation::GetPool();

  // Don't just use Pool::Allocate because that will use the first (lowest)
  // address with space.  The kernel's PMM initialization doesn't like the
  // earliest memory being split up too small, and anyway that's not very
  // representative of just a normal machine with some device memory elsewhere,
  // which is what the test RAM reservation is really meant to simulate.
  // Instead, find the highest-addressed, most likely large chunk that is big
  // enough and just make it a little smaller, which is probably more like what
  // an actual machine with a little less RAM would look like.

  auto it = pool.end();
  while (true) {
    if (it == pool.begin()) {
      break;
    }
    --it;
    if (it->type == memalloc::Type::kFreeRam && it->size >= ram.size) {
      uint64_t aligned_start = (it->addr + it->size - ram.size) & -uint64_t{ZX_PAGE_SIZE};
      uint64_t aligned_end = aligned_start + ram.size;
      if (aligned_start >= it->addr && aligned_end <= aligned_start + ram.size) {
        if (pool.UpdateRamSubranges(memalloc::Type::kTestRamReserve, aligned_start, ram.size)
                .is_ok()) {
          ram.paddr = aligned_start;
          debugf("%s: kernel.test.ram.reserve carve-out: [%#" PRIx64 ", %#" PRIx64 ")\n",
                 ProgramName(), aligned_start, aligned_end);
          return;
        }
        // Don't try another spot if something went wrong.
        break;
      }
    }
  }

  printf("%s: ERROR: Cannot reserve %#" PRIx64
         " bytes of RAM for kernel.test.ram.reserve request!\n",
         ProgramName(), ram.size);
}

// Returns a pointer into the array that was passed by reference.
constexpr ktl::string_view VmoNameString(const PhysVmo::Name& name) {
  ktl::string_view str(name.data(), name.size());
  return str.substr(0, str.find_first_of('\0'));
}

}  // namespace

void HandoffPrep::Init() {
  PhysHandoffTemporaryPtr<const PhysHandoff> handoff;
  fbl::AllocChecker ac;
  handoff_ = New(handoff, ac);
  ZX_ASSERT_MSG(ac.check(), "Failed to allocate PhysHandoff!");
}

PhysVmo HandoffPrep::MakePhysVmo(ktl::span<const ktl::byte> data, ktl::string_view name,
                                 size_t content_size) {
  uintptr_t addr = reinterpret_cast<uintptr_t>(data.data());
  ZX_ASSERT((addr % ZX_PAGE_SIZE) == 0);
  ZX_ASSERT((data.size_bytes() % ZX_PAGE_SIZE) == 0);
  ZX_ASSERT(((content_size + ZX_PAGE_SIZE - 1) & -ZX_PAGE_SIZE) == data.size_bytes());

  PhysVmo vmo{.addr = addr, .content_size = content_size};
  vmo.set_name(name);
  return vmo;
}

void HandoffPrep::SetInstrumentation() {
  auto publish_debugdata = [this](ktl::string_view sink_name, ktl::string_view vmo_name,
                                  ktl::string_view vmo_name_suffix, size_t content_size) {
    PhysVmo::Name phys_vmo_name =
        instrumentation::DebugdataVmoName(sink_name, vmo_name, vmo_name_suffix, /*is_static=*/true);

    size_t aligned_size = (content_size + ZX_PAGE_SIZE - 1) & -ZX_PAGE_SIZE;
    fbl::AllocChecker ac;
    ktl::span contents =
        Allocation::New(ac, memalloc::Type::kPhysDebugdata, aligned_size, ZX_PAGE_SIZE).release();
    ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu bytes for instrumentation phys VMO",
                  aligned_size);
    PublishExtraVmo(MakePhysVmo(contents, VmoNameString(phys_vmo_name), content_size));
    return contents;
  };
  for (const ElfImage* module : gSymbolize->modules()) {
    module->PublishDebugdata(publish_debugdata);
  }
}

void HandoffPrep::PublishExtraVmo(PhysVmo&& vmo) {
  fbl::AllocChecker ac;
  HandoffVmo* handoff_vmo = new (gPhysNew<memalloc::Type::kPhysScratch>, ac) HandoffVmo;
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu scratch bytes for HandoffVmo",
                sizeof(*handoff_vmo));

  handoff_vmo->vmo = ktl::move(vmo);
  extra_vmos_.push_front(handoff_vmo);
}

void HandoffPrep::FinishExtraVmos() {
  ZX_ASSERT_MSG(extra_vmos_.size() <= PhysVmo::kMaxExtraHandoffPhysVmos,
                "Too many phys VMOs in hand-off! %zu > max %zu", extra_vmos_.size(),
                PhysVmo::kMaxExtraHandoffPhysVmos);

  fbl::AllocChecker ac;
  ktl::span extra_phys_vmos = New(handoff()->extra_vmos, ac, extra_vmos_.size());
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu * %zu-byte PhysVmo", extra_vmos_.size(),
                sizeof(PhysVmo));
  ZX_DEBUG_ASSERT(extra_phys_vmos.size() == extra_vmos_.size());

  for (PhysVmo& phys_vmo : extra_phys_vmos) {
    phys_vmo = ktl::move(extra_vmos_.pop_front()->vmo);
  }
}

void HandoffPrep::SetMemory() {
  // TODO(https://fxbug.dev/355731771): Bootloaders and boot shims should be
  // providing a PERIPHERAL range that already covers UART MMIO, but there is
  // currently a gap in that coverage.
  if constexpr (kArchHandoffGenerateUartPeripheralRanges) {
    uart::internal::Visit(
        [&](auto& driver) {
          if (auto uart_mmio = GetUartMmioRange(driver, ZX_PAGE_SIZE)) {
            ZX_ASSERT(Allocation::GetPool().MarkAsPeripheral(*uart_mmio).is_ok());
          }
        },
        gBootOptions->serial);
  }

  // Normalizes types so that only those that are of interest to the kernel
  // remain.
  auto normed_type = [](memalloc::Type type) -> ktl::optional<memalloc::Type> {
    switch (type) {
      // The allocations that should survive into the hand-off.
      case memalloc::Type::kDataZbi:
      case memalloc::Type::kKernel:
      case memalloc::Type::kKernelPageTables:
      case memalloc::Type::kPhysDebugdata:
      case memalloc::Type::kPeripheral:
      case memalloc::Type::kPhysLog:
      case memalloc::Type::kReservedLow:
      case memalloc::Type::kTemporaryPhysHandoff:
      case memalloc::Type::kTestRamReserve:
      case memalloc::Type::kUserboot:
      case memalloc::Type::kVdso:
        return type;

      // The identity map needs to be installed at the time of hand-off, but
      // shouldn't actually be used by the kernel after that; mark it for
      // clean-up.
      case memalloc::Type::kTemporaryIdentityPageTables:
        return memalloc::Type::kTemporaryPhysHandoff;

      // An NVRAM range should no longer be treated like normal RAM. The kernel
      // will access it through PhysHandoff::nvram via its own mapping for it.
      //
      // TODO(https://fxbug.dev/42164859): Create a dedicated mapping for the
      // NVRAM range and hand that off.
      case memalloc::Type::kNvram:
      // Truncations should now go into effect.
      case memalloc::Type::kTruncatedRam:
        return ktl::nullopt;

      default:
        ZX_DEBUG_ASSERT(type != memalloc::Type::kReserved);
        break;
    }

    if (memalloc::IsRamType(type)) {
      return memalloc::Type::kFreeRam;
    }

    // Anything unknown should be ignored.
    return ktl::nullopt;
  };

  auto& pool = Allocation::GetPool();

  // Iterate through once to determine how many normalized ranges there are,
  // informing our allocation of its storage in the handoff.
  size_t len = 0;
  auto count_ranges = [&len](const memalloc::Range& range) {
    ++len;
    return true;
  };
  memalloc::NormalizeRanges(pool, count_ranges, normed_type);

  // Note, however, that New() has allocation side-effects around the creation
  // of temporary hand-off memory. Accordingly, overestimate the length by one
  // possible ranges when allocating the array, and adjust it after the fact.

  fbl::AllocChecker ac;
  ktl::span handoff_ranges = New(handoff()->memory, ac, len + 1);
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu bytes for memory handoff",
                len * sizeof(memalloc::Range));

  // Now simply record the normalized ranges.
  auto it = handoff_ranges.begin();
  auto record_ranges = [&it](const memalloc::Range& range) {
    *it++ = range;
    return true;
  };
  memalloc::NormalizeRanges(pool, record_ranges, normed_type);

  handoff()->memory.size_ = it - handoff_ranges.begin();
  handoff_ranges = ktl::span(handoff_ranges.begin(), it);

  if (gBootOptions->phys_verbose) {
    printf("%s: Physical memory handed off to the kernel:\n", ProgramName());
    memalloc::PrintRanges(handoff_ranges, ProgramName());
  }
}

BootOptions& HandoffPrep::SetBootOptions(const BootOptions& boot_options) {
  fbl::AllocChecker ac;
  BootOptions* handoff_options = New(handoff()->boot_options, ac, *gBootOptions);
  ZX_ASSERT_MSG(ac.check(), "cannot allocate handoff BootOptions!");

  if (handoff_options->test_ram_reserve) {
    FindTestRamReservation(*handoff_options->test_ram_reserve);
  }

  return *handoff_options;
}

void HandoffPrep::PublishLog(ktl::string_view name, Log&& log) {
  if (log.empty()) {
    return;
  }

  const size_t content_size = log.size_bytes();
  Allocation buffer = ktl::move(log).TakeBuffer();
  ZX_ASSERT(content_size <= buffer.size_bytes());

  PublishExtraVmo(MakePhysVmo(buffer.data(), name, content_size));

  // Intentionally leak as the PhysVmo now tracks this memory.
  ktl::ignore = buffer.release();
}

void HandoffPrep::UsePackageFiles(KernelStorage::Bootfs kernel_package) {
  auto& pool = Allocation::GetPool();
  for (auto it = kernel_package.begin(); it != kernel_package.end(); ++it) {
    ktl::span data = it->data;
    uintptr_t start = reinterpret_cast<uintptr_t>(data.data());
    // These are decompressed BOOTFS payloads, so there is only padding up to
    // the next page boundary.
    ktl::span aligned_data{data.data(), (data.size_bytes() + ZX_PAGE_SIZE - 1) & -ZX_PAGE_SIZE};
    if (it->name == "version-string.txt"sv) {
      ktl::string_view version{reinterpret_cast<const char*>(data.data()), data.size()};
      SetVersionString(version);
    } else if (it->name == "vdso"sv) {
      ZX_ASSERT(pool.UpdateRamSubranges(memalloc::Type::kVdso, start, data.size()).is_ok());
      handoff_->vdso = MakePhysVmo(aligned_data, "vdso/next"sv, data.size());
    } else if (it->name == "userboot"sv) {
      ZX_ASSERT(pool.UpdateRamSubranges(memalloc::Type::kUserboot, start, data.size()).is_ok());
      handoff_->userboot = MakePhysVmo(aligned_data, "userboot"sv, data.size());
    }
  }
  if (auto result = kernel_package.take_error(); result.is_error()) {
    zbitl::PrintBootfsError(result.error_value());
  }
  ZX_ASSERT_MSG(!handoff_->version_string.empty(), "no version.txt file in kernel package");
}

void HandoffPrep::SetVersionString(ktl::string_view version) {
  constexpr ktl::string_view kSpace = " \t\r\n";
  size_t skip = version.find_first_not_of(kSpace);
  size_t trim = version.find_last_not_of(kSpace);
  if (skip == ktl::string_view::npos || trim == ktl::string_view::npos) {
    ZX_PANIC("version.txt of %zu chars empty after trimming whitespace", version.size());
  }
  trim = version.size() - (trim + 1);
  version.remove_prefix(skip);
  version.remove_suffix(trim);

  fbl::AllocChecker ac;
  ktl::string_view installed = New(handoff_->version_string, ac, version);
  if (!ac.check()) {
    ZX_PANIC("cannot allocate %zu chars of handoff space for version string", version.size());
  }
  ZX_ASSERT(installed == version);
  if (gBootOptions->phys_verbose) {
    if (skip + trim == 0) {
      printf("%s: zx_system_get_version_string (%zu chars): %.*s\n", ProgramName(), version.size(),
             static_cast<int>(version.size()), version.data());
    } else {
      printf("%s: zx_system_get_version_string (%zu chars trimmed from %zu): %.*s\n", ProgramName(),
             version.size(), version.size() + skip + trim, static_cast<int>(version.size()),
             version.data());
    }
  }
}

[[noreturn]] void HandoffPrep::DoHandoff(UartDriver& uart, ktl::span<ktl::byte> zbi,
                                         const KernelStorage::Bootfs& kernel_package,
                                         const ArchPatchInfo& patch_info,
                                         fit::inline_function<void(PhysHandoff*)> boot) {
  // Hand off the boot options first, which don't really change.  But keep a
  // mutable reference to update boot_options.serial later to include live
  // driver state and not just configuration like other BootOptions members do.
  BootOptions& handoff_options = SetBootOptions(*gBootOptions);

  // Use the updated copy from now on.
  gBootOptions = &handoff_options;

  UsePackageFiles(kernel_package);

  SummarizeMiscZbiItems(zbi);
  gBootTimes.SampleNow(PhysBootTimes::kZbiDone);

  ArchHandoff(patch_info);

  SetInstrumentation();

  // This transfers the log, so logging after this is not preserved.
  // Extracting the log buffer will automatically detach it from stdout.
  // TODO(mcgrathr): Rename to physboot.log with some prefix.
  PublishLog("i/logs/physboot", ktl::move(*ktl::exchange(gLog, nullptr)));

  // Finalize the published VMOs, including the log just published above.
  FinishExtraVmos();

  // Now that all time samples have been collected, copy gBootTimes into the
  // hand-off.
  handoff()->times = gBootTimes;

  // This finalizes the state of memory to hand off to the kernel, which is affected by other set-up
  // routines.
  SetMemory();

  // Hand-off the serial driver. There may be no more logging beyond this point.
  handoff()->uart = ktl::move(uart).TakeUart();

  boot(handoff());
  ZX_PANIC("HandoffPrep::DoHandoff boot function returned!");
}
