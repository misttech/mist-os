// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TLS_H_
#define LIB_LD_TLS_H_

#include <lib/stdcompat/bit.h>

#include "abi.h"

namespace ld::abi {

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

}  // namespace ld::abi

#endif  // LIB_LD_TLS_H_
