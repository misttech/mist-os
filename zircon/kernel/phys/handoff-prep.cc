// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "handoff-prep.h"

#include <ctype.h>
#include <lib/boot-options/boot-options.h>
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
#include <phys/allocation.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/main.h>
#include <phys/new.h>
#include <phys/symbolize.h>

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
        if (pool.UpdateFreeRamSubranges(memalloc::Type::kTestRamReserve, aligned_start, ram.size)
                .is_ok()) {
          ram.paddr = aligned_start;
          if (gBootOptions->phys_verbose) {
            // Dump out the memory usage again to show the reservation.
            printf("%s: Physical memory after kernel.test.ram.reserve carve-out:\n", ProgramName());
            pool.PrintMemoryRanges(ProgramName());
          }
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

// Construct a VMO name for the instrumentation data from the debugdata
// protocol sink name, the module name and the canonical file name suffix for
// this sink.
constexpr PhysVmo::Name DebugdataVmoName(ktl::string_view sink_name, ktl::string_view vmo_name,
                                         ktl::string_view vmo_name_suffix) {
  PhysVmo::Name name{};
  ktl::span<char> buffer{name};
  // TODO(fxbug.dev/120396)): Currently this matches the old "data/phys/..."
  // format and doesn't contain the sink name.  Later change maybe to
  // "i/sink-name/module+suffix" but omit whole suffix if it doesn't all fit?
  for (ktl::string_view str : {"data/phys/"sv, vmo_name, vmo_name_suffix}) {
    buffer = buffer.subspan(str.copy(buffer.data(), buffer.size() - 1));
    if (buffer.empty()) {
      break;
    }
  }
  return name;
}

// Returns a pointer into the array that was passed by reference.
constexpr ktl::string_view VmoNameString(const PhysVmo::Name& name) {
  ktl::string_view str(name.data(), name.size());
  return str.substr(0, str.find_first_of('\0'));
}

}  // namespace

void HandoffPrep::Init(ktl::span<ktl::byte> buffer) {
  // TODO(fxbug.dev/84107): Use the buffer inside the data ZBI via a
  // SingleHeapAllocator.  Later allocator() will return a real(ish) allocator.
  allocator_.allocate_function() = AllocateFunction(buffer);

  fbl::AllocChecker ac;
  handoff_ = new (allocator(), ac) PhysHandoff;
  ZX_ASSERT_MSG(ac.check(), "handoff buffer too small for PhysHandoff!");
}

void HandoffPrep::SetInstrumentation() {
  auto publish_debugdata = [this](ktl::string_view sink_name, ktl::string_view vmo_name,
                                  ktl::string_view vmo_name_suffix, size_t content_size) {
    PhysVmo::Name phys_vmo_name = DebugdataVmoName(sink_name, vmo_name, vmo_name_suffix);
    return PublishVmo(VmoNameString(phys_vmo_name), content_size);
  };
  for (const ElfImage* module : gSymbolize->modules()) {
    module->PublishDebugdata(publish_debugdata);
  }
}

ktl::span<ktl::byte> HandoffPrep::PublishVmo(ktl::string_view name, size_t content_size) {
  if (content_size == 0) {
    return {};
  }

  fbl::AllocChecker ac;
  HandoffVmo* handoff_vmo = new (gPhysNew<memalloc::Type::kPhysScratch>, ac) HandoffVmo;
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu scratch bytes for HandoffVmo",
                sizeof(*handoff_vmo));

  handoff_vmo->vmo.set_name(name);

  ktl::span buffer = New(handoff_vmo->vmo.data, ac, content_size);
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu bytes for %.*s", content_size,
                static_cast<int>(name.size()), name.data());
  ZX_DEBUG_ASSERT(buffer.size() == content_size);

  vmos_.push_front(handoff_vmo);
  return buffer;
}

void HandoffPrep::FinishVmos() {
  fbl::AllocChecker ac;
  ktl::span phys_vmos = New(handoff()->vmos, ac, vmos_.size());
  ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu * %zu-byte PhysVmo", vmos_.size(),
                sizeof(PhysVmo));
  ZX_DEBUG_ASSERT(phys_vmos.size() == vmos_.size());

  for (PhysVmo& phys_vmo : phys_vmos) {
    phys_vmo = ktl::move(vmos_.pop_front()->vmo);
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

  ktl::span copy = PublishVmo(name, content_size);
  ZX_ASSERT(copy.size_bytes() == content_size);
  memcpy(copy.data(), buffer.get(), content_size);
}

void HandoffPrep::UsePackageFiles(const KernelStorage::Bootfs& kernel_package) {
  SetVersionString(kernel_package);
}

void HandoffPrep::SetVersionString(KernelStorage::Bootfs kernel_package) {
  // Fetch the version-string.txt file from the package.
  ktl::string_view version;
  if (auto it = kernel_package.find("version-string.txt"); it == kernel_package.end()) {
    if (auto result = kernel_package.take_error(); result.is_error()) {
      zbitl::PrintBootfsError(result.error_value());
    }
    ZX_PANIC("no version.txt file in kernel package");
  } else {
    version = {reinterpret_cast<const char*>(it->data.data()), it->data.size()};
  }
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

  ArchHandoff();

  SetInstrumentation();

  // This transfers the log, so logging after this is not preserved.
  // Extracting the log buffer will automatically detach it from stdout.
  // TODO(mcgrathr): Rename to physboot.log with some prefix.
  PublishLog("data/phys/symbolizer.log", ktl::move(*ktl::exchange(gLog, nullptr)));

  // Finalize the published VMOs, including the log just published above.
  FinishVmos();

  // Now that all time samples have been collected, copy gBootTimes into the
  // hand-off.
  handoff()->times = gBootTimes;

  // Copy any post-Init() serial state from the live driver here in physboot
  // into the handoff BootOptions.  There should be no more printing from here
  // on.  TODO(fxbug.dev/84107): Actually there is some printing in BootZbi,
  // but no current drivers carry post-Init() state so it's harmless for now.
  uart.Visit([&handoff_options](const auto& driver) { handoff_options.serial = driver.uart(); });

  boot(handoff());
  ZX_PANIC("HandoffPrep::DoHandoff boot function returned!");
}
