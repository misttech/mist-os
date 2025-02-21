// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/devicetree/devicetree.h>

#include <fbl/alloc_checker.h>
#include <phys/address-space.h>
#include <phys/allocation.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include "third_party/modp_b64/modp_b64.h"

namespace {

constexpr const char* kShimName = "devicetree-dump-shim";

}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();

  AddressSpace aspace;
  InitMemory(flat_devicetree_blob, &aspace);
  MainSymbolize symbolize(kShimName);

  // At this point UART should be available, and we should just encode.
  devicetree::ByteView fdt_blob(static_cast<const uint8_t*>(flat_devicetree_blob),
                                std::numeric_limits<uintptr_t>::max());
  devicetree::Devicetree fdt(fdt_blob);

  size_t base64_size = modp_b64_encode_len(fdt.size_bytes());
  fbl::AllocChecker checker;
  Allocation base64_buffer = Allocation::New(checker, memalloc::Type::kPhysScratch, base64_size);
  if (!checker.check()) {
    printf("Failed to allocate buffer(size=%#zx) for the base64 encoding of the devicetree.",
           base64_size);
  } else {
    size_t encode_len =
        modp_b64_encode(reinterpret_cast<char*>(base64_buffer.data().data()),
                        reinterpret_cast<const char*>(fdt.fdt().data()), fdt.size_bytes());
    if (encode_len < 0) {
      printf("Failed to encode devicetree.\n");
    } else {
      printf("Devicetree Base64 Dump Begin encoded_bytes=%zu\n", encode_len);
      printf("%.*s\n", static_cast<int>(encode_len),
             reinterpret_cast<const char*>(base64_buffer.data().data()));
      printf("\nDevicetree Base64 Dump End\n");
    }
  }
  abort();
}
