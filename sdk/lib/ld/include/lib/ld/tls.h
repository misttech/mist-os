// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TLS_H_
#define LIB_LD_TLS_H_

#include <lib/stdcompat/bit.h>

#include <cstddef>

#include "abi.h"

namespace ld {
namespace abi {

// This describes the details gleaned from the PT_TLS header for a module.
// These are stored in an array indexed by TLS module ID number - 1, as the
// module ID number zero is never used.
//
// Note that while module ID number 1 is most often the main executable, that
// need not always be so: if the main executable has no PT_TLS of its own, then
// the earliest module loaded that does have a PT_TLS gets module ID 1.
//
// What is importantly special about the main executable is that offsets in the
// static TLS block are chosen with the main executable first--it may have been
// linked with Local Exec TLS access code where the linker chose its expected
// offsets at static link time.  When the dynamic linker follows the usual
// procedure of assigning module IDs in load order and then doing static TLS
// layout in the same order, it always comes out the same.  But the only real
// constraint on the runtime layout chosen is that if the main executable has a
// PT_TLS segment, it must be first and its offset from the thread pointer must
// be the fixed value prescribed by the psABI.  The adjacent private portions
// of the runtime thread descriptor must be located such that both their own
// alignment requirements and the p_align of module 1's PT_TLS are respected.

template <class Elf, class AbiTraits>
struct Abi<Elf, AbiTraits>::TlsModule {
  constexpr typename Elf::size_type tls_size() const {
    return tls_initial_data.size() + tls_bss_size;
  }

  // Initial data image in memory, usually a pointer into the RODATA or RELRO
  // segment of the module's load image.
  Span<const std::byte> tls_initial_data;

  // If the module has a PT_TLS, its total size in memory (for each thread) is
  // determined by the initial data (tls_initial_data.size_bytes(), from .tdata
  // et al) plus this size of zero-initialized bytes (from .tbss et al).
  Addr tls_bss_size = 0;

  // The runtime memory for each thread's copy of the initialized PT_TLS data
  // for this segment must have at least this minimum alignment (p_align).
  // This is validated to be a power of two before the module is loaded.
  Addr tls_alignment = 0;
};

}  // namespace abi

// Fetch the current thread pointer with the given byte offset.
template <typename T = void>
inline T* TpRelative(ptrdiff_t offset = 0) {
  std::byte* tp;
#if defined(__x86_64__) && defined(__clang__)
  // This fetches %fs:0, but the compiler knows what it's doing.  LLVM knows
  // that in the compiler ABI %fs:0 always stores the %fs.base address, and its
  // optimizer will see through this to integrate *TpRelative(N) as a direct
  // "mov %fs:N, ...".  Note that these special pointer types can be used to
  // access memory, but they cannot be cast to a normal pointer type (which in
  // the abstract should add in the base address, but the compiler doesn't know
  // how to do that).
  using FsRelative = std::byte* [[clang::address_space(257)]];
  tp = *reinterpret_cast<FsRelative*>(0);
#elif defined(__x86_64__)
  // TODO(mcgrathr): GCC 6 supports this syntax instead (and __seg_gs):
  //     void* __seg_fs* fs = 0;
  // Unfortunately, it allows it only in C and not in C++.
  // It also requires -fasm under -std=c11 (et al), see:
  //     https://gcc.gnu.org/bugzilla/show_bug.cgi?id=79609
  // It's also buggy for the special case of 0, see:
  //     https://gcc.gnu.org/bugzilla/show_bug.cgi?id=79619
  __asm__ __volatile__("mov %%fs:0,%0" : "=r"(tp));
#elif defined(__i386__) && defined(__clang__)
  // Everything above applies the same on x86-32, but with %gs instead.
  using GsRelative = std::byte* [[clang::address_space(256)]];
  tp = *reinterpret_cast<GsRelative*>(0);
#elif defined(__i386__)
  __asm__ __volatile__("mov %%gs:0,%0" : "=r"(tp));
#else
  tp = static_cast<std::byte*>(__builtin_thread_pointer());
#endif

  return reinterpret_cast<T*>(tp + offset);
}

// Return the given pointer's byte offset from the thread pointer.
// `TpRelative(TpRelativeToOffset(ptr)) == ptr` should always be true.
template <typename T>
inline ptrdiff_t TpRelativeToOffset(T* ptr) {
  std::byte* tp = TpRelative<std::byte>();
  return reinterpret_cast<std::byte*>(ptr) - tp;
}

}  // namespace ld

#endif  // LIB_LD_TLS_H_
