// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_STARTUP_BOOTSTRAP_H_
#define LIB_LD_STARTUP_BOOTSTRAP_H_

#include <lib/ld/abi.h>
#include <lib/ld/bootstrap.h>
#include <lib/ld/module.h>

namespace ld {

struct StartupBootstrap : public ld::Bootstrap {
  template <typename PageSizeT>
  StartupBootstrap(auto& diag, const void* vdso_base, PageSizeT&& page_size)
      : ld::Bootstrap{diag, vdso_base, std::forward<PageSizeT>(page_size),
                      // See below about the module storage.
                      (gSelfModule.InitLinkerZeroInitialized(), gSelfModule),
                      (gVdsoModule.InitLinkerZeroInitialized(), gVdsoModule)} {}

  // We want these objects to be in bss to reduce the amount of data pages
  // which need COW.  In general the only data/bss we want should be part of
  // `_ld_abi`, but the ld.so and vDSO modules will always be in the `_ld_abi`
  // list so it is safe to keep these objects in .bss.  They will be protected
  // to read-only later.  The explicit .bss section attribute ensures this
  // object is zero initialized, we will get an assembler error otherwise.

  [[gnu::section(".bss.self_module")]] constinit static inline abi::Abi<>::Module gSelfModule =
      abi::Abi<>::Module::LinkerZeroInitialized();

  [[gnu::section(".bss.vdso_module")]] constinit static inline abi::Abi<>::Module gVdsoModule =
      abi::Abi<>::Module::LinkerZeroInitialized();
};

// This determines the whole-page bounds of the RELRO + data + bss segment.
// (LLD uses a layout with two contiguous segments, but that's equivalent.)
// After startup, protect all of this rather than just the RELRO region.
// Use like: `auto [start, size] = DataBounds(page_size);`
struct DataBounds {
  DataBounds() = delete;

  explicit DataBounds(size_t page_size)
      : start(PageRound(kStart, page_size)),      // Page above RO.
        size(PageRound(kEnd, page_size) - start)  // Page above RW.
  {}

  uintptr_t start;
  size_t size;

 private:
  // These are actually defined implicitly by the linker: _etext is the limit
  // of the read-only segments (code and/or RODATA), so the data starts on the
  // next page up; _end is the limit of the bss, which implicitly extends to
  // the end of that page.
  [[gnu::visibility("hidden")]] static std::byte kStart[] __asm__("_etext");
  [[gnu::visibility("hidden")]] static std::byte kEnd[] __asm__("_end");

  static uintptr_t PageRound(void* ptr, size_t page_size) {
    return (reinterpret_cast<uintptr_t>(ptr) + page_size - 1) & -page_size;
  }
};

// TODO(https://fxbug.dev/42080826): After LlvmProfdata:UseCounters, functions will load
// the new value of __llvm_profile_counter_bias and use it. However, functions
// already in progress will use a cached value from before it changed. This
// means they'll still be pointing into the data segment and updating the old
// counters there. So they'd crash with write faults if it were protected.
// There may be a way to work around this by having uninstrumented functions
// call instrumented functions such that the tail return path of any frame live
// across the transition is uninstrumented. Note that each function will
// resample even if that function is inlined into a caller that itself will
// still be using the stale pointer. However, in the long run we expect to move
// from the relocatable-counters design to a new design where the counters are
// in a separate "bss-like" location that we arrange to be in a separate VMO
// created by the program loader. If we do that, then this issue won't arise,
// so we might not get around to making protecting the data compatible with
// profdata instrumentation before it's moot.
inline constexpr bool kProtectData = !HAVE_LLVM_PROFDATA;

}  // namespace ld

#endif  // LIB_LD_STARTUP_BOOTSTRAP_H_
