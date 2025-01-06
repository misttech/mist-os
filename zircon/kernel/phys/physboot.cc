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
#include <phys/address-space.h>
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

ChainBoot LoadZirconZbi(KernelStorage::Bootfs kernelfs, const ArchPatchInfo& patch_info) {
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

  // There's always the self-test patch, so there should never be none.
  ZX_ASSERT(!patcher.patches().empty());
  for (const code_patching::Directive& patch : patcher.patches()) {
    ZX_ASSERT(patch.range_start >= KERNEL_LINK_ADDRESS);
    ZX_ASSERT(patch.range_size <= kernel_bytes.size());
    ZX_ASSERT(kernel_bytes.size() - patch.range_size >= patch.range_start - KERNEL_LINK_ADDRESS);

    ktl::span<ktl::byte> insns =
        kernel_bytes.subspan(patch.range_start - KERNEL_LINK_ADDRESS, patch.range_size);

    auto print = [patch](ktl::initializer_list<ktl::string_view> strings) {
      printf("%s: code-patching: ", ProgramName());
      for (ktl::string_view str : strings) {
        stdout->Write(str);
      }
      printf(": [%#" PRIx64 ", %#" PRIx64 ")\n", patch.range_start,
             patch.range_start + patch.range_size);
    };

    if (!ArchPatchCode(patcher, patch_info, insns, static_cast<CodePatchId>(patch.id), print)) {
      ZX_PANIC("%s: code-patching: unrecognized patch case ID: %" PRIu32 ": [%#" PRIx64
               ", %#" PRIx64 ")",
               ProgramName(), patch.id, patch.range_start, patch.range_start + patch.range_size);
    }
  }

  debugf("%s: Examining ZBI...\n", gSymbolize->name());
  BootZbi::InputZbi kernel_zbi(kernel_bytes);

  if (kernel_bytes.size_bytes() > KERNEL_IMAGE_MAX_SIZE) {
    ZX_PANIC(
        "physboot: Attempting to load kernel of size %#zx. Max supported kernel size is %#zx (\"KERNEL_IMAGE_MAX_SIZE\").\n",
        kernel_bytes.size_bytes(), static_cast<size_t>(KERNEL_IMAGE_MAX_SIZE));
  }

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

void FreeDataZbi(BootZbi& boot) {
  ktl::span zbi = boot.DataZbi().storage();

  uint64_t data_start = reinterpret_cast<uintptr_t>(zbi.data());
  uint64_t data_end = data_start + zbi.size();

#ifdef __x86_64__
  uint64_t kernel_start = TrampolineBoot::kLegacyLoadAddress;
  uint64_t kernel_end = kernel_start + boot.KernelMemorySize();
  if (kernel_start <= data_start && data_start < kernel_end) {
    data_start = ktl::min(kernel_end, data_end);
  }
#endif
  ZX_ASSERT(Allocation::GetPool().Free(data_start, data_end - data_start).is_ok());
}

}  // namespace

[[noreturn]] void BootZircon(UartDriver& uart, KernelStorage kernel_storage) {
  KernelStorage::Bootfs package = kernel_storage.GetKernelPackage();
  const ArchPatchInfo patch_info = ArchPreparePatchInfo();
  ChainBoot boot = LoadZirconZbi(package, patch_info);

  // Repurpose the storage item as a place to put the handoff payload.
  KernelStorage::Zbi::iterator handoff_item = kernel_storage.item();

  // `boot`'s data ZBI at this point is the tail of the decompressed kernel
  // ZBI; overwrite that with the original data ZBI, making sure to free any
  // previous allocation as well.
  FreeDataZbi(boot);
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

    FreeDataZbi(boot);
    boot.DataZbi() = ktl::move(relocated_image);
    handoff_item = it;
  }

  ktl::span zbi = boot.DataZbi().storage();

// TODO(https://fxbug.dev/42164859): It's far easier to construct the kernel
// mapping here than it is in the kernel's start.S, the second of which encodes
// load address assumptions that are on track to be invalidated. Removing the
// start.S logic enables us to more easily translate to ELF-loading the x86
// kernel, at which point this whole file will be deleted.
#ifdef __x86_64__
  constexpr uint64_t kX86LegacyVirtLoadAddr = 0xffff'ffff'0010'0000;
  constexpr AddressSpace::MapSettings kRwx{
      .access = {.readable = true, .writable = true, .executable = true}};
  uint64_t aligned_memory_size = (boot.KernelMemorySize() + ZX_PAGE_SIZE - 1) & -ZX_PAGE_SIZE;
  AddressSpace::PanicIfError(gAddressSpace->Map(kX86LegacyVirtLoadAddr, aligned_memory_size,
                                                TrampolineBoot::kLegacyLoadAddress, kRwx));
#endif

  if (gBootOptions->phys_verbose) {
    boot.Log();
  }

  // Prepare the handoff data structures.
  HandoffPrep prep;
  prep.Init();

  prep.DoHandoff(uart, zbi, package, patch_info, [&boot](PhysHandoff* handoff) {
    // Even though the kernel is still a ZBI and mostly using the ZBI protocol
    // for booting, the PhysHandoff pointer (physical address) is now the
    // argument to the kernel, not the data ZBI address.
    boot.Boot(handoff);
  });
}
