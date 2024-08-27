// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_H_

// This file specifies the private ABI shared between userboot and the kernel.
// That is, the contents of the message sent on userboot's bootstrap channel.

#include <lib/instrumentation/vmo.h>

#include <cstdint>

#include <phys/handoff.h>

namespace userboot {

// This is only here for the count.  No userboot code cares which is which
// except that the stable (default) variant is first and that kLastVdso (below)
// is correct.
enum class VdsoVariant { STABLE, NEXT, TEST1, TEST2, COUNT };

// The handles in the bootstrap message are as follows:
enum HandleIndex : uint32_t {
  // These describe userboot itself.
  kProcSelf,
  kVmarRootSelf,

  // Essential job and resource handles.
  kRootJob,
  kMmioResource,
  kIrqResource,
#if __x86_64__
  kIoportResource,
#elif __aarch64__
  kSmcResource,
#endif
  kSystemResource,

  // Essential VMO handles.
  kZbi,

  kFirstVdso,
  kLastVdso = kFirstVdso + static_cast<uint32_t>(VdsoVariant::COUNT) - 1,

  // These get passed along to userland to be recognized by ZX_PROP_NAME.
  // The remainder are VMO handles that userboot doesn't care about.
  kCrashlog,
  kFirstKernelFile = kCrashlog,

  kBootOptions,

  kCounterNames,
  kCounters,

  kTimeValues,
#if ENABLE_ENTROPY_COLLECTOR_TEST
  kEntropyTestData,
#endif

  kFirstInstrumentationData,
  kFirstExtraPhysVmo = kFirstInstrumentationData + InstrumentationData::vmo_count(),
  kHandleCount = kFirstExtraPhysVmo + PhysVmo::kMaxExtraHandoffPhysVmos,
};

// Copied from sdk/lib/fdio/include/lib/fdio/io.h to avoid the dependency. When this is passed
// with a PA_FD handle, the handle is tied to stdout.
constexpr uint32_t kFdioFlagUseForStdio = 0x8000;

// Max number of bytes allowed for arguments to the userboot.next binary. This is an arbitrary
// value.
constexpr uint32_t kProcessArgsMaxBytes = 128;

}  // namespace userboot

// Called at the end of the boot process in the main kernel initialization sequence.
void userboot_init();

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_H_
