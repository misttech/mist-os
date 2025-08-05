// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/code-patching/self-test.h>
#include <lib/elfldltl/machine.h>
#include <lib/uart/all.h>
#include <stdlib.h>
#include <zircon/assert.h>

#include <phys/handoff.h>
#include <phys/zircon-abi-spec.h>
#include <phys/zircon-info-note.h>

namespace {

constexpr ZirconAbiSpec kZirconAbiSpec{
    .machine_stack =
        {
            .size_bytes = 0x1000,
            .lower_guard_size_bytes = 0x1000,
            .upper_guard_size_bytes = 0x1000,
        },
#if __has_feature(shadow_call_stack)
    .shadow_call_stack =
        {
            .size_bytes = 0x1000,
            .lower_guard_size_bytes = 0x1000,
            .upper_guard_size_bytes = 0x1000,
        },
#endif
};

ZIRCON_INFO_NOTE ZirconInfoNote<kZirconAbiSpec> kZirconAbiSpecNote;

}  // namespace

PhysHandoff* gPhysHandoff = nullptr;

void PhysbootHandoff(PhysHandoff* handoff) {
  // Check that the stack is aligned.
  uintptr_t stack_pointer = reinterpret_cast<uintptr_t>(__builtin_frame_address(0));
  ZX_ASSERT((stack_pointer & (elfldltl::AbiTraits<>::kStackAlignment<uintptr_t> - 1)) == 0);

  // Temporary hand-off pointer dereferencing checks that this is set.
  gPhysHandoff = handoff;

  uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy>(
      handoff->boot_options->serial)
      .Visit([](auto& uart) {
        uart.Write("Hello world!\n");
        CodePatchingNopTest();
        uart.Write("I've been patched!\n");
        uart.Write("\n" BOOT_TEST_SUCCESS_STRING "\n");
      });
  abort();
}

// This is what ZX_ASSERT calls.
void __zx_panic(const char* format, ...) { __builtin_trap(); }

// This is what libc++ headers call.
[[noreturn]] void std::__libcpp_verbose_abort(const char* format, ...) noexcept {
  __builtin_trap();
}
