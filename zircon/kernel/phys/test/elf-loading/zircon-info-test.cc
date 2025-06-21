// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "zircon-info-test.h"

#include <lib/zbitl/error-stdio.h>

#include <cstdint>

#include <phys/address-space.h>
#include <phys/elf-image.h>
#include <phys/kernel-package.h>
#include <phys/symbolize.h>

#include "../physload-test-main.h"
#include "get-int.h"

#include <ktl/enforce.h>

namespace {

constexpr const char* kTestName = "zircon-info-test";

// The name of ELF module to be loaded.
constexpr ktl::string_view kGetInt = "get-int.zircon-info-test";

// TODO(https://fxbug.dev/42172722): Pick a load address through a sort of allocator
// (i.e., as we would in production).
constexpr uint64_t kLoadAddress = 0xffff'ffff'0000'0000;

}  // namespace

int PhysLoadTestMain(KernelStorage& kernelfs) {
  gSymbolize->set_name(kTestName);

  printf("Loading %.*s...\n", static_cast<int>(kGetInt.size()), kGetInt.data());
  ElfImage elf;
  if (auto result = elf.Init(kernelfs.root(), kGetInt, true); result.is_error()) {
    zbitl::PrintBootfsError(result.error_value());
    return 1;
  }

  ZX_ASSERT(!elf.has_patches());

  // The GN target for get-int uses kernel_elf_interp() on this test binary.
  printf("Verifying PT_INTERP matches test build ID...\n");
  elf.AssertInterpMatchesBuildId(kGetInt, gSymbolize->build_id());

  printf("Checking ZirconInfo note...\n");
  ZX_ASSERT(elf.zircon_info());
  ktl::optional info = elf.GetZirconInfo<ZirconInfoTest>();
  ZX_ASSERT(info);
  ZX_ASSERT(info->x == 17);
  ZX_ASSERT(info->y == 23);

  Allocation loaded = elf.Load(memalloc::Type::kPhysElf, kLoadAddress);
  elf.Relocate();

  if (auto result = elf.MapInto(*gAddressSpace); result.is_error()) {
    printf("Failed to map loaded image\n");
    return 1;
  }

  printf("Calling virtual entry point %#" PRIx64 "...\n", elf.entry());

  // We should now be able to access GetInt()!
  constexpr int kExpected = 42;
  if (int actual = elf.Call<decltype(GetInt)>(); actual != kExpected) {
    printf("FAILED: Expected %d; got %d\n", kExpected, actual);
    return 1;
  }

  return 0;
}
