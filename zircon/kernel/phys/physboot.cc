// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "physboot.h"

#include <inttypes.h>
#include <lib/arch/zbi-boot.h>
#include <lib/boot-options/boot-options.h>
#include <lib/code-patching/code-patches.h>
#include <lib/code-patching/code-patching.h>
#include <lib/memalloc/range.h>
#include <lib/zbitl/error-stdio.h>
#include <stdio.h>
#include <zircon/assert.h>

#include <ktl/move.h>
#include <phys/allocation.h>
#include <phys/boot-zbi.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include "handoff-prep.h"
#include "log.h"

#ifdef __x86_64__
#include <phys/trampoline-boot.h>

using ChainBoot = TrampolineBoot;

#else

using ChainBoot = BootZbi;

#endif

#include <ktl/enforce.h>

namespace {

// A guess about the upper bound on reserve_memory_size so we can do a single
// allocation before decoding the header and probably not need to relocate.
constexpr uint64_t kKernelBssEstimate = 1024 * 1024 * 2;

ChainBoot LoadZirconZbi(KernelStorage::Bootfs kernelfs) {
  // Now we select our kernel ZBI.
  debugf("%s: Locating ZBI file in kernel package...\n", gSymbolize->name());
  auto it = kernelfs.find(kKernelZbiName);
  if (auto result = kernelfs.take_error(); result.is_error()) {
    printf("physboot: Error in looking for kernel ZBI within STORAGE_KERNEL item: ");
    zbitl::PrintBootfsError(result.error_value());
    abort();
  }
  if (it == kernelfs.end()) {
    printf("physboot: Could not find kernel ZBI (%.*s/%.*s) within STORAGE_KERNEL item\n",
           static_cast<int>(kernelfs.directory().size()), kernelfs.directory().data(),
           static_cast<int>(kKernelZbiName.size()), kKernelZbiName.data());
    abort();
  }
  ktl::span<ktl::byte> kernel_bytes = {const_cast<ktl::byte*>(it->data.data()), it->data.size()};

  debugf("%s: Found %zu-byte kernel ZBI, locating patches...\n", gSymbolize->name(),
         kernel_bytes.size());

  // Patch the kernel image in the BOOTFS in place before loading it.
  code_patching::Patcher patcher;
  if (auto result = patcher.Init(kernelfs); result.is_error()) {
    printf("physboot: Failed to initialize code patching: ");
    code_patching::PrintPatcherError(result.error_value());
    abort();
  }
  debugf("%s: Applying %zu patches...\n", gSymbolize->name(), patcher.patches().size());
  ArchPatchCode(ktl::move(patcher), kernel_bytes, KERNEL_LINK_ADDRESS);

  debugf("%s: Examining ZBI...\n", gSymbolize->name());
  BootZbi::InputZbi kernel_zbi(kernel_bytes);
  ChainBoot boot;
  if (auto result = boot.Init(kernel_zbi); result.is_error()) {
    printf("physboot: Cannot read STORAGE_KERNEL item ZBI: ");
    zbitl::PrintViewCopyError(result.error_value());
    abort();
  }

  debugf("%s: Loading ZBI kernel...\n", gSymbolize->name());
  if (auto result = boot.Load(kKernelBssEstimate); result.is_error()) {
    printf("physboot: Cannot load decompressed kernel: ");
    zbitl::PrintViewCopyError(result.error_value());
    abort();
  }

  return boot;
}

}  // namespace

[[noreturn]] void BootZircon(UartDriver& uart, KernelStorage kernel_storage) {
  debugf("%s: Finding kernel package...\n", gSymbolize->name());
  KernelStorage::Bootfs kernelfs;
  if (auto result = kernel_storage.root().subdir(kDefaultKernelPackage); result.is_error()) {
    printf("physboot: Failed to read kernel package %.*s: ",
           static_cast<int>(kDefaultKernelPackage.size()), kDefaultKernelPackage.data());
    zbitl::PrintBootfsError(result.error_value());
    abort();
  } else {
    kernelfs = ktl::move(result).value();
  }
  ChainBoot boot = LoadZirconZbi(kernelfs);

  // Repurpose the storage item as a place to put the handoff payload.
  KernelStorage::Zbi::iterator handoff_item = kernel_storage.item();

  // `boot`'s data ZBI at this point is the tail of the decompressed kernel
  // ZBI; overwrite that with the original data ZBI.
  boot.DataZbi() = kernel_storage.zbi();

  Allocation relocated_zbi;
  if (boot.MustRelocateDataZbi()) {
    // Actually, the original data ZBI must be moved elsewhere since it
    // overlaps the space where the fixed-address kernel will be loaded.
    fbl::AllocChecker ac;
    relocated_zbi =
        Allocation::New(ac, memalloc::Type::kDataZbi, kernel_storage.zbi().storage().size(),
                        arch::kZbiBootDataAlignment);
    if (!ac.check()) {
      printf("physboot: Cannot allocate %#zx bytes aligned to %#zx for relocated data ZBI!\n",
             kernel_storage.zbi().storage().size(), arch::kZbiBootDataAlignment);
      abort();
    }
    if (auto result = kernel_storage.zbi().Copy(relocated_zbi.data(), kernel_storage.zbi().begin(),
                                                kernel_storage.zbi().end());
        result.is_error()) {
      kernel_storage.zbi().ignore_error();
      printf("physboot: Failed to relocate data ZBI: ");
      zbitl::PrintViewCopyError(result.error_value());
      printf("\n");
      abort();
    }
    ZX_ASSERT(kernel_storage.zbi().take_error().is_ok());

    // Rediscover the handoff item's new location in memory.
    ChainBoot::Zbi relocated_image(relocated_zbi.data());
    auto it = relocated_image.begin();
    while (it != relocated_image.end() && it.item_offset() < handoff_item.item_offset()) {
      ++it;
    }
    ZX_ASSERT(it != relocated_image.end());
    ZX_ASSERT(relocated_image.take_error().is_ok());

    boot.DataZbi() = ktl::move(relocated_image);
    handoff_item = it;
  }

  // Prepare the handoff data structures.
  HandoffPrep prep;
  prep.Init(handoff_item->payload);

  // Hand off the boot options first, which don't really change.  But keep a
  // mutable reference to update boot_options.serial later to include live
  // driver state and not just configuration like other BootOptions members do.
  BootOptions& handoff_options = prep.SetBootOptions(*gBootOptions);

  // Use the updated copy from now on.
  gBootOptions = &handoff_options;

  prep.SummarizeMiscZbiItems(boot.DataZbi().storage());
  gBootTimes.SampleNow(PhysBootTimes::kZbiDone);

  prep.SetInstrumentation();

  // This transfers the log, so logging after this is not preserved.
  // Extracting the log buffer will automatically detach it from stdout.
  // TODO(mcgrathr): Rename to physboot.log with some prefix.
  prep.PublishLog("data/phys/symbolizer.log", ktl::move(*ktl::exchange(gLog, nullptr)));

  // Finalize the published VMOs, including the log just published above.
  prep.FinishVmos();

  // Now that all time samples have been collected, copy gBootTimes into the
  // hand-off.
  prep.handoff()->times = gBootTimes;

  // Copy any post-Init() serial state from the live driver here in physboot
  // into the handoff BootOptions.  There should be no more printing from here
  // on.  TODO(fxbug.dev/84107): Actually there is some printing in BootZbi,
  // but no current drivers carry post-Init() state so it's harmless for now.
  uart.Visit([&handoff_options](const auto& driver) { handoff_options.serial = driver.uart(); });

  // Even though the kernel is still a ZBI and mostly using the ZBI protocol
  // for booting, the PhysHandoff pointer (physical address) is now the
  // argument to the kernel, not the data ZBI address.
  boot.Boot(prep.handoff());
}
