// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_ABI_H_
#define LIB_LD_ABI_H_

// This defines a common "passive" ABI that runtime code like a C library can
// use to interrogate basic dynamic linking details.  It's called a "passive"
// ABI because it exports almost no entry points, but only some immutable data
// structures and the ELF symbol names by which to find them.
//
// The traditional PT_INTERP dynamic linker sets up this data in memory while
// doing the initial-exec dynamic linking, and then makes it all read-only so
// it's guaranteed never to change again.  The runtime dynamic linking support
// (-ldl) can ingest this data into its own data structures and manage those
// to provide a richer runtime ABI.  Basic fallback implementations of simple
// support calls like dl_iterate_phdr and dlsym can be provided by the C
// library when libdl.so is not linked in.
//
// For out-of-process dynamic linking, a simple stub implementation of this
// same ABI can be loaded in lieu of the traditional dynamic linker, giving
// the same simple runtime ABI for data that is populated out of process.

#include <lib/elfldltl/abi-ptr.h>
#include <lib/elfldltl/abi-span.h>
#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/svr4-abi.h>
#include <lib/elfldltl/symbol.h>

#include <string_view>

namespace ld::abi {

template <class Elf = elfldltl::Elf<>, class AbiTraits = elfldltl::LocalAbiTraits>
struct Abi {
  // Aliases to avoid using `typename` all over the place.
  using Word = typename Elf::Word;
  using Addr = typename Elf::Addr;
  using Phdr = typename Elf::Phdr;
  using LinkMap = typename Elf::template LinkMap<AbiTraits>;
  using RDebug = typename Elf::template RDebug<AbiTraits>;

  // All pointer members in Abi and inner types must use Ptr<T> in place of T*.
  template <typename T>
  using Ptr = elfldltl::AbiPtr<T, Elf, AbiTraits>;

  // Members in Abi and inner types use Span<T> in place of std::span<T>.
  template <typename T, size_t N = cpp20::dynamic_extent>
  using Span = elfldltl::AbiSpan<T, N, Elf, AbiTraits>;

  // This is a shorthand for instantiating the elfldltl types that take Elf and
  // AbiTraits template parameters, such as elfldltl::SymbolInfo.
  template <template <class, class> class Class>
  using Type = Class<Elf, AbiTraits>;

  // Forward declaration for type declared in module.h.
  struct Module;

  // This lists all the initial-exec modules.  Embedded `link_map::l_prev` and
  // `link_map::l_next` form a doubly-linked list in load order, which is a
  // breadth-first pre-order of the DT_NEEDED dependencies where the main
  // executable is always first and dependents always precede dependencies
  // (except for any redundancies).
  Ptr<const Module> loaded_modules;

  // TODO(fxbug.dev/128502): TLS layout details
};

// This is the DT_SONAME value representing the ABI declared in this file.
inline constexpr elfldltl::Soname<> kSoname{"ld.so.1"};

// This is the standard PT_INTERP value for using a compatible dynamic linker
// as the startup dynamic linker.  The actual PT_INTERP value in an executable
// ET_DYN file might have a prefix to select a particular implementation.
inline constexpr std::string_view kInterp = kSoname.str();

// These are the sole exported symbols, with hash values cached statically.
inline constexpr elfldltl::SymbolName kAbiSymbol{"_ld_abi"};
inline constexpr elfldltl::SymbolName kRDebugSymbol{"_r_debug"};

// These are the sole exported symbols in the ld.so ABI.  They should be used
// in C++ via their scoped names such as ld::_ld_abi normally.  But the ld.so
// symbolic ABI does not include any C++ name mangling, so they use simple C
// linkage names in the name space reserved for the implementation.

extern "C" {

// This is the single hook to find the ld.so passive ABI described above.
extern const Abi<> _ld_abi;

// This is not meant to be used as part of the passive ABI, really.  However,
// traditionally debuggers expect to find it as a symbol in the dynamic linker,
// and exporting it makes sure they can even without debugging symbols.
extern const Abi<>::RDebug _r_debug;

}  // extern "C"

}  // namespace ld::abi

#endif  // LIB_LD_ABI_H_
