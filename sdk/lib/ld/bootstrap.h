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
#include <lib/ld/module.h>

#include <cstddef>
#include <cstdint>
#include <limits>

namespace ld {

// This fills out all the fields of a Module except the linked-list pointers.
// The Module describes the vDSO, which is already fully loaded according to
// its PT_LOAD segments, relocated and initialized in place as we find it.  The
// ELF image as loaded is presumed valid.  The diagnostics object will be used
// for assertion failures in case messages can be printed, but it's not really
// expected to return from FormatError et al and various kinds of failures
// might get crashes without or after FormatError returns.  It will usually be
// used with some kind of TrapDiagnostics() or PanicDiagnostics() object.
//
// The optional argument should be the system runtime page size.  If it's not
// given, then the returned Module's vaddr_start and and vaddr_end will not be
// properly page-aligned.  In that case, CompleteVdsoModule (below) should be
// called once the page size is known.
template <class Diagnostics>
inline abi::Abi<>::Module BootstrapVdsoModule(Diagnostics&& diag, const void* vdso_base,
                                              size_t page_size = 1) {
  using Ehdr = elfldltl::Elf<>::Ehdr;
  using Phdr = elfldltl::Elf<>::Phdr;
  using Dyn = elfldltl::Elf<>::Dyn;
  using size_type = elfldltl::Elf<>::size_type;

  elfldltl::DirectMemory memory(
      {
          static_cast<std::byte*>(const_cast<void*>(vdso_base)),
          std::numeric_limits<size_t>::max(),
      },
      0);

  const Ehdr& ehdr = *memory.ReadFromFile<Ehdr>(0);
  const cpp20::span phdrs =
      *memory.ReadArrayFromFile<Phdr>(ehdr.phoff, elfldltl::NoArrayFromFile<Phdr>{}, ehdr.phnum);

  size_type vaddr_start, vaddr_size;
  std::optional<elfldltl::ElfNote> build_id;
  std::optional<Phdr> dyn_phdr;
  elfldltl::DecodePhdrs(
      diag, phdrs, elfldltl::PhdrLoadObserver<elfldltl::Elf<>>(page_size, vaddr_start, vaddr_size),
      elfldltl::PhdrDynamicObserver<elfldltl::Elf<>>(dyn_phdr),
      elfldltl::PhdrMemoryNoteObserver(elfldltl::Elf<>{}, memory,
                                       elfldltl::ObserveBuildIdNote(build_id)));

  const cpp20::span dyn = *memory.ReadArray<Dyn>(dyn_phdr->vaddr, dyn_phdr->memsz);

  const size_type bias = reinterpret_cast<uintptr_t>(vdso_base) - vaddr_start;
  abi::Abi<>::Module vdso = {
      .link_map = {.addr{bias}, .ld{dyn.data()}},
      .vaddr_start = vaddr_start,
      .vaddr_end = vaddr_start + vaddr_size,
      .phdrs = phdrs,
      .build_id = build_id->desc,  // The vDSO always has a build ID.
  };

  elfldltl::DecodeDynamic(diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(vdso.symbols));
  vdso.soname = vdso.symbols.soname();
  vdso.link_map.name = vdso.soname.str().data();

  return vdso;
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
inline abi::Abi<>::Module BootstrapSelfModule(Diagnostics&& diag, const abi::Abi<>::Module& vdso) {
  auto memory = elfldltl::Self<>::Memory();
  const cpp20::span phdrs = elfldltl::Self<>::Phdrs();

  std::optional<elfldltl::ElfNote> build_id;
  elfldltl::DecodePhdrs(diag, phdrs,
                        elfldltl::PhdrMemoryNoteObserver(elfldltl::Elf<>{}, memory,
                                                         elfldltl::ObserveBuildIdNote(build_id)));

  const uintptr_t bias = elfldltl::Self<>::LoadBias();
  const uintptr_t start = memory.base() + bias;
  const cpp20::span dyn = elfldltl::Self<>::Dynamic();
  abi::Abi<>::Module self = {
      .link_map = {.addr{bias}, .ld{dyn.data()}},
      .vaddr_start = start,
      .vaddr_end = start + memory.image().size(),
      .phdrs = phdrs,
      .symbols = elfldltl::LinkStaticPieWithVdso(elfldltl::Self<>(), diag, vdso.symbols,
                                                 vdso.link_map.addr),
  };

  self.soname = self.symbols.soname();
  self.link_map.name = self.soname.str().data();
  self.build_id = build_id->desc;

  return self;
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
