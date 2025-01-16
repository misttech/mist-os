// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "physboot.h"

#include <inttypes.h>
#include <lib/arch/cache.h>
#include <lib/boot-options/boot-options.h>
#include <lib/code-patching/code-patches.h>
#include <lib/fit/function.h>
#include <lib/zbitl/error-stdio.h>
#include <stdlib.h>
#include <zircon/assert.h>

#include <arch/code-patches/case-id.h>
#include <arch/kernel_aspace.h>
#include <ktl/initializer_list.h>
#include <ktl/move.h>
#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/boot-zbi.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include "handoff-prep.h"
#include "physload.h"

#include <ktl/enforce.h>

PhysBootTimes gBootTimes;

namespace {

constexpr ktl::string_view kElfPhysKernel = "physzircon";

void PatchElfKernel(ElfImage& elf_kernel, const ArchPatchInfo& patch_info) {
  auto apply_patch = [&patch_info](
                         code_patching::Patcher& patcher, CodePatchId id,
                         ktl::span<ktl::byte> code_to_patch,
                         ElfImage::PrintPatchFunction print) -> fit::result<ElfImage::Error> {
    if (ArchPatchCode(patcher, patch_info, code_to_patch, id, ktl::move(print))) {
      return fit::ok();
    }
    print({"unrecognized patch case ID"});
    ZX_PANIC("%s: code-patching: unrecognized patch case ID %" PRIu32, gSymbolize->name(),
             static_cast<uint32_t>(id));
  };

  debugf("%s: Applying %zu patches...\n", gSymbolize->name(), elf_kernel.patch_count());
  // Apply patches to the kernel image.
  auto result = elf_kernel.ForEachPatch<CodePatchId>(apply_patch);
  if (result.is_error()) {
    zbitl::PrintBootfsError(result.error_value());
    abort();
  }

  // There's always the self-test patch, so there should never be none.
  ZX_ASSERT(elf_kernel.has_patches());
}

void RelocateElfKernel(ElfImage& elf_kernel) {
  debugf("%s: Relocating ELF kernel to [%#" PRIx64 ", %#" PRIx64 ")...\n", gSymbolize->name(),
         elf_kernel.load_address(), elf_kernel.load_address() + elf_kernel.vaddr_size());
  elf_kernel.Relocate();
}

[[noreturn]] void BootZircon(UartDriver& uart, KernelStorage kernel_storage) {
  KernelStorage::Bootfs package = kernel_storage.GetKernelPackage();

  ElfImage elf_kernel;
  debugf("%s: Locating ELF kernel in kernel package...\n", gSymbolize->name());
  if (auto result = elf_kernel.Init(package, kElfPhysKernel, true); result.is_error()) {
    printf("%s: Cannot load ELF kernel \"%.*s/%.*s\" from STORAGE_KERNEL item BOOTFS: ",
           gSymbolize->name(), static_cast<int>(package.directory().size()),
           package.directory().data(), static_cast<int>(kElfPhysKernel.size()),
           kElfPhysKernel.data());
    zbitl::PrintBootfsError(result.error_value());
    abort();
  }

  // Make sure the kernel was built to match this physboot binary.
  elf_kernel.AssertInterpMatchesBuildId(gSymbolize->name(), gSymbolize->build_id());

  // Use the putative eventual virtual address to relocate the kernel.
  const uint64_t kernel_vaddr = kArchHandoffVirtualAddress;

  Allocation loaded_elf_kernel = elf_kernel.Load(memalloc::Type::kKernel, kernel_vaddr);

  const ArchPatchInfo patch_info = ArchPreparePatchInfo();
  PatchElfKernel(elf_kernel, patch_info);

  RelocateElfKernel(elf_kernel);

  if (elf_kernel.memory_image().size_bytes() > KERNEL_IMAGE_MAX_SIZE) {
    ZX_PANIC(
        "%s: Attempting to load kernel of size %#zx. Max supported kernel size is %#zx (\"KERNEL_IMAGE_MAX_SIZE\").\n",
        gSymbolize->name(), elf_kernel.memory_image().size_bytes(),
        static_cast<size_t>(KERNEL_IMAGE_MAX_SIZE));
  }

  // Prepare the handoff data structures.
  HandoffPrep prep;
  prep.Init();

  // Set up the kernel's final page protections.
  //
  // TODO(https://fxbug.dev/42164859): Eventually we'll want to hand off more
  // metadata relating to the mapped sections of the image, at which point this
  // call should be reframed into machinery that updates PhysHandoff as well.
  AddressSpace::PanicIfError(elf_kernel.MapInto(*gAddressSpace));

  // For now we're loading an ELF kernel in physical address mode at an
  // arbitrary load address, even though it's been relocated for its final
  // virtual address.  The kernel's entry point is expected to be purely
  // position independent long enough to switch to virtual addressing.
  //
  // NOTE: For real handoff with virtual addresses, this will need some inline
  // asm to switch stacks and such. For interim hack kernels doing physical
  // address mode handoff, they can either use the phys stack momentarily
  // or have asm entry code that sets up its own stack.
  elf_kernel.set_load_address(elf_kernel.physical_load_address());
  debugf("%s: Ready to hand off at physical load address %#" PRIxPTR ", entry %#" PRIx64 "...\n",
         gSymbolize->name(), elf_kernel.load_address(), elf_kernel.entry());
  if (gBootOptions->phys_verbose) {
    Allocation::GetPool().PrintMemoryRanges(gSymbolize->name());
  }

  auto start_elf_kernel = [&elf_kernel](PhysHandoff* handoff) {

  // TODO(https://fxbug.dev/42164859): Hand off virtual address space for ARM
  // too.
#ifdef __aarch64__
    // This runs in an identity-mapped environment, so the MMU can be safely
    // turned off.  The physzircon kernel entry code expects the MMU to be off.
    arch::DisableMmu();
#endif
    elf_kernel.Handoff<void(PhysHandoff*)>(handoff);
  };
  prep.DoHandoff(uart, kernel_storage.zbi().storage(), package, patch_info, start_elf_kernel);
}

}  // namespace

[[noreturn]] void PhysLoadModuleMain(UartDriver& uart, PhysBootTimes boot_times,
                                     KernelStorage kernel_storage) {
  gBootTimes = boot_times;

  gSymbolize->set_name("physboot");

  // Now we're ready for the main physboot logic.
  BootZircon(uart, ktl::move(kernel_storage));
}
