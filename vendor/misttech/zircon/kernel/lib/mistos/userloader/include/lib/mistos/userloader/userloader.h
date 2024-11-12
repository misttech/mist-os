// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_USERLOADER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_USERLOADER_H_

// This file specifies the private ABI shared between userloader and the kernel.

#include <lib/instrumentation/vmo.h>
#include <lib/mistos/zx/vmo.h>

#include <cstdint>

#include <ktl/array.h>

namespace userloader {

// The handles in the bootstrap message are as follows:
enum HandleIndex : uint32_t {
  // These describe userboot itself.
  kProcSelf,
  kVmarRootSelf,

  // Essential job and resource handles.
  kRootJob,
  kRootResource,
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

  // These get passed along to userland to be recognized by ZX_PROP_NAME.
  // The remainder are VMO handles that userloader doesn't care about.
  kCrashlog,
  kFirstKernelFile = kCrashlog,

  kBootOptions,

  kCounterNames,
  kCounters,
#if ENABLE_ENTROPY_COLLECTOR_TEST
  kEntropyTestData,
#endif

  kFirstInstrumentationData,
  kHandleCount = kFirstInstrumentationData + InstrumentationData::vmo_count()
};

// Max number of bytes allowed for arguments to the userboot.next binary. This is an arbitrary
// value.
constexpr uint32_t kProcessArgsMaxBytes = 128;

// Global Vmos handles
extern ktl::array<Handle*, userloader::kHandleCount> gHandles;

}  // namespace userloader

#endif  // ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_USERLOADER_H_
