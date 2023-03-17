// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/code-patching/self-test.h>
#include <lib/uart/all.h>
#include <stdlib.h>
#include <zircon/assert.h>

#include <phys/handoff.h>

void PhysbootHandoff(PhysHandoff* handoff) {
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

template <>
void* PhysHandoffPtrImportPhysAddr<PhysHandoffPtrEncoding::PhysAddr>(uintptr_t ptr) {
  return reinterpret_cast<void*>(ptr);
}

// This is what ZX_ASSERT calls.
void __zx_panic(const char* format, ...) { __builtin_trap(); }

// This is what libc++ headers call.
[[noreturn]] void std::__libcpp_verbose_abort(const char* format, ...) { __builtin_trap(); }
