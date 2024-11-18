// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_BOOTSTRAP_H_
#define LIB_LD_BOOTSTRAP_H_

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/self.h>
#include <lib/elfldltl/static-pie-with-vdso.h>
#include <lib/elfldltl/symbol.h>
#include <lib/ld/load.h>
#include <lib/ld/module.h>

#include <cstddef>
#include <cstdint>
#include <limits>

namespace ld {

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

struct BootstrapModule {
  abi::Abi<>::Module& module;
  std::span<const elfldltl::Elf<>::Dyn> dyn;
};

inline BootstrapModule FinishBootstrapModule(abi::Abi<>::Module& module,
                                             std::span<const elfldltl::Elf<>::Dyn> dyn,
                                             size_t vaddr_start, size_t vaddr_size, size_t bias,
                                             std::span<const elfldltl::Elf<>::Phdr> phdrs) {
  module.link_map.addr = bias;
  module.link_map.ld = dyn.data();
  module.vaddr_start = vaddr_start;
  module.vaddr_end = vaddr_start + vaddr_size;
  module.phdrs = phdrs;
  module.soname = module.symbols.soname();
  module.link_map.name = module.soname.str().data();
  return {module, dyn};
}

// This fills out all the fields of a Module except the linked-list pointers.
// The Module describes the vDSO, which is already fully loaded according to
// its PT_LOAD segments, relocated and initialized in place as we find it.  The
// ELF image as loaded is presumed valid.  The diagnostics object will be used
// for assertion failures in case messages can be printed, but it's not really
// expected to return from FormatError et al and various kinds of failures
// might get crashes without or after FormatError returns.  It will usually be
// used with some kind of TrapDiagnostics() or similar object.
//
// The optional argument should be the system runtime page size.  If it's not
// given, then the returned Module's vaddr_start and and vaddr_end will not be
// properly page-aligned.  In that case, CompleteVdsoModule (below) should be
// called once the page size is known.
template <class Diagnostics>
inline BootstrapModule BootstrapVdsoModule(Diagnostics&& diag, const void* vdso_base,
                                           size_t page_size = 1) {
  using Ehdr = elfldltl::Elf<>::Ehdr;
  using Phdr = elfldltl::Elf<>::Phdr;
  using Dyn = elfldltl::Elf<>::Dyn;
  using size_type = elfldltl::Elf<>::size_type;

  // We want this object to be in bss to reduce the amount of data pages which
  // need COW.  In general the only data/bss we want should be part of
  // `_ld_abi`, but the vdso module will always be in the `_ld_abi` list so it
  // is safe to keep this object in .bss.  It will be protected to read-only
  // later.  The explicit .bss section attribute ensures this object is zero
  // initialized, we will get an assembler error otherwise.
  [[gnu::section(".bss.vdso_module")]] constinit static abi::Abi<>::Module vdso =
      abi::Abi<>::Module::LinkerZeroInitialized();
  vdso.InitLinkerZeroInitialized();

#ifndef __Fuchsia__
  if (!vdso_base) [[unlikely]] {
    // If there is no vDSO, then there will just be empty symbols to link
    // against and no references can resolve to any vDSO-defined symbols.
    // This will on1y ever be true on Posix, never on Fuchsia.
    return {vdso, {}};
  }
#endif

  elfldltl::DirectMemory memory(
      {
          static_cast<std::byte*>(const_cast<void*>(vdso_base)),
          std::numeric_limits<size_t>::max(),
      },
      0);

  const Ehdr& ehdr = *memory.ReadFromFile<Ehdr>(0);
  const std::span phdrs =
      *memory.ReadArrayFromFile<Phdr>(ehdr.phoff, elfldltl::NoArrayFromFile<Phdr>{}, ehdr.phnum);

  size_type vaddr_start, vaddr_size;
  std::optional<Phdr> dyn_phdr;
  elfldltl::DecodePhdrs(
      diag, phdrs, elfldltl::PhdrLoadObserver<elfldltl::Elf<>>(page_size, vaddr_start, vaddr_size),
      elfldltl::PhdrDynamicObserver<elfldltl::Elf<>>(dyn_phdr),
      PhdrMemoryBuildIdObserver(memory, vdso));

  const std::span dyn = *memory.ReadArray<Dyn>(dyn_phdr->vaddr, dyn_phdr->memsz);
  elfldltl::DecodeDynamic(diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(vdso.symbols));

  const size_type bias = reinterpret_cast<uintptr_t>(vdso_base) - vaddr_start;

  return FinishBootstrapModule(vdso, dyn, vaddr_start, vaddr_size, bias, phdrs);
}

// This bootstraps this dynamic linker itself, doing its own dynamic linking
// for simple fixups and for symbolic references resolved in the vDSO module
// from BootstrapVdsoModule(), above. As with that function, the program's own
// ELF image is presumed valid and its PT_LOAD segments correctly loaded; the
// diagnostics object is used for assertion failures, but not expected to
// return after errors.  This fills out all the fields of a Module except the
// linked-list pointers.  The Module describes this dynamic linker itself,
// already loaded and now fully relocated; RELRO pages remain writable.  The
// Module's vaddr_start and vaddr_end are not properly page-aligned until
// CompleteBootstrapModule (below) is called.
template <class Diagnostics>
inline BootstrapModule BootstrapSelfModule(Diagnostics&& diag, const abi::Abi<>::Module& vdso) {
  using Phdr = elfldltl::Elf<>::Phdr;
  using Dyn = elfldltl::Elf<>::Dyn;

  auto memory = elfldltl::Self<>::Memory();
  const std::span phdrs = elfldltl::Self<>::Phdrs();

  // We want this object to be in bss to reduce the amount of data pages which
  // need COW.  In general the only data/bss we want should be part of
  // `_ld_abi`, but the self module will always be in the `_ld_abi` list so it
  // is safe to keep this object in .bss.  It will be protected to read only
  // later.  The explicit .bss section attribute ensures this object is zero
  // initialized, we will get an assembler error otherwise.
  [[gnu::section(".bss.self_module")]] constinit static abi::Abi<>::Module self =
      abi::Abi<>::Module::LinkerZeroInitialized();
  // Note, this call could be elided because it only sets `symbols` which will
  // be immediately replaced.  In case this function changes to do more we
  // should keep the call.  The compiler should be smart enough to figure out
  // this is a dead store.
  self.InitLinkerZeroInitialized();

  std::optional<Phdr> dyn_phdr;
  elfldltl::DecodePhdrs(  //
      diag, phdrs, elfldltl::PhdrDynamicObserver<elfldltl::Elf<>>(dyn_phdr),
      PhdrMemoryBuildIdObserver(memory, self));

  const uintptr_t bias = elfldltl::Self<>::LoadBias();
  const uintptr_t start = memory.base() + bias;
  std::span dyn = elfldltl::Self<>::Dynamic();

  self.symbols =
      elfldltl::LinkStaticPieWithVdso(elfldltl::Self<>(), diag, vdso.symbols, vdso.link_map.addr);

  dyn = dyn.subspan(0, dyn_phdr->memsz / sizeof(Dyn));
  return FinishBootstrapModule(self, dyn, start, memory.image().size(), bias, phdrs);
}

inline void CompleteBootstrapModule(abi::Abi<>::Module& module, size_t page_size) {
  module.vaddr_start = module.vaddr_start & -page_size;
  module.vaddr_end = (module.vaddr_end + page_size - 1) & -page_size;
}

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

}  // namespace ld

#endif  // LIB_LD_BOOTSTRAP_H_
