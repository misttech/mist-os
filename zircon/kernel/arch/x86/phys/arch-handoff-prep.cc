// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/code-patching/code-patches.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbi-format/zbi.h>
#include <zircon/assert.h>

#include <ktl/byte.h>
#include <ktl/span.h>
#include <phys/arch/arch-handoff.h>
#include <phys/handoff.h>

#include "handoff-prep.h"

#include <ktl/enforce.h>

ArchPatchInfo ArchPreparePatchInfo() { return {}; }

void HandoffPrep::ArchSummarizeMiscZbiItem(const zbi_header_t& header,
                                           ktl::span<const ktl::byte> payload) {
  switch (header.type) {
    case ZBI_TYPE_FRAMEBUFFER:
      SaveForMexec(header, payload);
      break;
  }
}

void HandoffPrep::ArchConstructKernelAddressSpace() {}

void HandoffPrep::ArchDoHandoff(ZirconAbi abi, const ArchPatchInfo& patch_info) {
  ZX_DEBUG_ASSERT_MSG(!abi.shadow_call_stack_base, "Shadow call stack not supported on x86");

  __asm__ volatile(
      // We want the kernel's main to be at the root of the call stack, so
      // clear the frame pointer.
      "xor %%ebp, %%ebp\n"

      // TODO(https://fxbug.dev/42164859): Set or clear the would-be unsafe stack pointer
      // TODO(https://fxbug.dev/42164859): Set the thread pointer.
      "mov %[rsp], %%rsp\n"

      // The kernel's C++ entrypoint is allowed to assume that it's in the cld
      // state.
      "cld\n"

      "jmpq *%[entry]"
      :                                    //
      : [entry] "r"(kernel_.entry()),      //
        [handoff] "D"(handoff_),           // D" places it in %rdi
        [rsp] "r"(abi.machine_stack_top),  //
        "m"(*handoff_)  // Ensures no store to the handoff can be regarded as dead
  );
  __UNREACHABLE;
}
