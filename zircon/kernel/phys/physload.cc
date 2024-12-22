// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "physload.h"

#include <lib/boot-options/boot-options.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/zbitl/error-stdio.h>
#include <lib/zbitl/view.h>

#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include "log.h"

#include <ktl/enforce.h>

namespace {

// physload, physboot, EL2, kernel
constexpr size_t kMaxPhysloadModules = 4;

void LogSerial(FILE* out = stdout) {
  fprintf(out, "%s: Console configured as ", ProgramName());
  gBootOptions->Show("kernel.serial"sv, false, out);
}

}  // namespace

[[noreturn]] void ZbiMain(void* zbi_ptr, arch::EarlyTicks ticks) {
  PhysBootTimes times;
  times.Set(PhysBootTimes::kZbiEntry, ticks);

  MainSymbolize symbolize("physload");
  if (gBootOptions->phys_verbose) {
    symbolize.Context();
    LogSerial();
  }

  ArchPhysloadBeforeInitMemory();

  AddressSpace aspace;
  InitMemory(zbi_ptr, &aspace);

  // This marks the interval between handoff from the boot loader (kZbiEntry)
  // and phys environment setup with identity-mapped memory management et al.
  times.SampleNow(PhysBootTimes::kPhysSetup);

  // Start collecting the log in memory as well as logging to the console.
  Log log;

  {
    // Prime the log with what would already have been written to the console
    // under kernel.phys.verbose=true (even if it wasn't), but don't send that
    // to the console.
    FILE log_file{&log};
    symbolize.ContextAlways(&log_file);
    LogSerial(&log_file);
    Allocation::GetPool().PrintMemoryRanges(symbolize.name(), &log_file);
  }

  // Now mirror all stdout to the log, and write debugf there even if verbose
  // logging to stdout is disabled.
  gLog = &log;
  log.SetStdout();

  auto zbi_header = static_cast<zbi_header_t*>(zbi_ptr);
  auto zbi = zbitl::StorageFromRawHeader<ktl::span<ktl::byte>>(zbi_header);

  // Unpack the compressed KERNEL_STORAGE payload.
  KernelStorage kernel_storage;
  kernel_storage.Init(zbitl::View{zbi});
  kernel_storage.GetTimes(times);

  // TODO: add some time samples around bootfs decoding, loading, etc.
  KernelStorage::Bootfs bootfs = kernel_storage.root();

  // Provide space for loading modules.
  ktl::array<const ElfImage*, kMaxPhysloadModules> modules_storage;
  symbolize.ReplaceModulesStorage(Symbolize::ModuleList(modules_storage));

  ktl::string_view next_file_name = gBootOptions->phys_next.data();

  // Load up the next module.
  ElfImage next_elf;
  if (auto result = next_elf.Init(bootfs, next_file_name, true); result.is_error()) {
    zbitl::PrintBootfsError(result.error_value());
    abort();
  }

  // We don't support any code-patching for physboot - or any physload module
  // at the moment - though we could if there were any worth doing.
  ZX_ASSERT_MSG(!next_elf.has_patches(),
                "kernel.phys.next ELF image with code-patches not supported");

  // Load the image, in place if space or copied elsewhere if not.
  Allocation loaded = next_elf.Load(memalloc::Type::kPhysElf);

  // Relocate the image.
  next_elf.Relocate();

  next_elf.AssertInterpMatchesBuildId(symbolize.name(), symbolize.build_id());

  if (gBootOptions->phys_verbose) {
    symbolize.LogHandoff(next_elf.name(), next_elf.entry());
  }

  // Call into the entry point.  It must not return, but it will keep using the
  // same stack so it can safely take references to our stack objects.
  next_elf.Handoff<PhysLoadHandoffFunction>(next_elf, &log, gArchPhysInfo, GetUartDriver(),
                                            &symbolize, gBootOptions, Allocation::GetPool(),
                                            gAddressSpace, times, ktl::move(kernel_storage));
}
