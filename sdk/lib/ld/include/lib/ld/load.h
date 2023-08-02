// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_LOAD_H_
#define LIB_LD_LOAD_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/phdr.h>

#include <tuple>

#include "memory.h"
#include "module.h"

namespace ld {

// Several functions here fill in the passive ABI Module data structure,
// either after or while setting up LoadInfo and Memory objects.

// Shorthand.
template <class Elf = elfldltl::Elf<>>
using AbiModule = typename abi::Abi<Elf>::Module;

// Set the Module vaddr bounds and phdrs fields from the LoadInfo and runtime
// load bias.
template <class Elf = elfldltl::Elf<>, class LoadInfo, class Memory>
constexpr void SetModuleLoadInfo(AbiModule<Elf>& module, const typename Elf::Ehdr& ehdr,
                                 const LoadInfo& load_info, typename Elf::size_type load_bias,
                                 Memory& memory) {
  module.link_map.addr = load_bias;
  module.vaddr_start = load_info.vaddr_start() + load_bias;
  module.vaddr_end = module.vaddr_start + load_info.vaddr_size();

  // Find the segment that covers the range of the file occupied by the phdrs:
  // [phoff, phoff + phnum * sizeof(Phdr)), if there is one.  Note this is
  // doing a linear search, but in canonical ELF layouts the first segment in
  // the list is always the correct one so usually this will in fact be
  // optimal.  In general, however, there is no requirement that it be first,
  // or that any segment qualify: the segments are ordered by vaddr; while
  // usually offset increases as vaddr increases, that is not mandated by the
  // ELF format rules, so there's no way to avoid checking all the segments
  // until a qualifying one is found.
  load_info.VisitSegments([&](const auto& segment) {
    using Phdr = typename Elf::Phdr;
    if (segment.offset() <= ehdr.phoff && ehdr.phoff - segment.offset() < segment.filesz() &&
        (segment.filesz() - (ehdr.phoff - segment.offset())) / sizeof(Phdr) >= ehdr.phnum) {
      if (auto read_phdrs = memory.template ReadArray<Phdr>(
              ehdr.phoff - segment.offset() + segment.vaddr(), ehdr.phnum)) {
        module.phdrs = *read_phdrs;
      }
      return false;  // Found the segment of interest; stop looking.
    }
    return true;  // Keep looking at the next segment.
  });
}

template <class Elf = elfldltl::Elf<>>
struct ModulePhdrInfo {
  std::optional<typename Elf::Phdr> dyn_phdr;
  std::optional<typename Elf::size_type> stack_size;
};

// This uses elfldltl::DecodePhdrs for the things that can gleaned from phdrs
// before the whole load image is accessible in memory.  These are not stored
// directly in Module, so they are just returned in ModulePhdrInfo.  Additional
// phdr observers can be passed to include in the same initial scan.  This will
// include some elfldltl::LoadInfo<...>::GetPhdrObserver() observer, as well as
// whatever else the caller can use immediately.  Since phdrs refer to contents
// both via file offset and via vaddr, when the entire ELF file image is
// already accessible in memory, other things like notes can be examined in the
// same one pass using elfldltl::PhdrFileNoteObserver.
template <class Elf = elfldltl::Elf<>, class Diagnostics, typename... PhdrObservers>
constexpr ModulePhdrInfo<Elf> DecodeModulePhdrs(Diagnostics& diag,
                                                cpp20::span<const typename Elf::Phdr> phdrs,
                                                PhdrObservers&&... phdr_observers) {
  ModulePhdrInfo<Elf> result;
  elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(result.dyn_phdr),
                        elfldltl::PhdrStackObserver<Elf>(result.stack_size),
                        std::forward<PhdrObservers>(phdr_observers)...);
  return result;
}

// This uses elfldltl::DecodeDynamic to fill the Module fields.  Additional
// dynamic section observers can be passed to include in the same scan.
template <class Elf = elfldltl::Elf<>, class Diagnostics, class Memory,
          typename... DynamicObservers>
constexpr void DecodeModuleDynamic(AbiModule<Elf>& module, Diagnostics& diag, Memory& memory,
                                   const std::optional<typename Elf::Phdr>& dyn_phdr,
                                   DynamicObservers&&... dynamic_observers) {
  using Dyn = const typename Elf::Dyn;

  if (!dyn_phdr) [[unlikely]] {
    diag.FormatError("no PT_DYNAMIC program header found");
    return;
  }

  const size_t count = dyn_phdr->filesz / sizeof(Dyn);
  auto read_dyn = memory.template ReadArray<Dyn>(dyn_phdr->vaddr, count);
  if (!read_dyn) [[unlikely]] {
    diag.FormatError("cannot read", count, "entries from PT_DYNAMIC",
                     elfldltl::FileAddress{dyn_phdr->vaddr});
    return;
  }
  cpp20::span<const Dyn> dyn = *read_dyn;

  module.link_map.ld = dyn.data();

  elfldltl::DecodeDynamic(diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(module.symbols),
                          std::forward<DynamicObservers>(dynamic_observers)...);
  module.soname = module.symbols.soname();
}

// Return an observer object to be passed to elfldltl::*NoteObserver.  When
// that object is destroyed, it fills in the Module::build_id field.  This can
// be used either via elfldltl::PhdrFileNoteObserver in DecodeModulePhdrs, or
// via elfldltl::PhdrMemoryNoteObserver in a second elfldltl::DecodePhdrs scan
// performed after the module has been loaded.
template <class Elf = elfldltl::Elf<>>
constexpr auto ObserveBuildIdNote(AbiModule<Elf>& module, bool keep_going = false) {
  // Use the toolkit's generic observer to find the build ID note.
  using ObserverResult = std::optional<elfldltl::ElfNote>&;
  auto make_observer = [keep_going](ObserverResult& build_id) {
    return elfldltl::ObserveBuildIdNote(build_id, keep_going);
  };
  using NoteObserver = decltype(make_observer(std::declval<ObserverResult>()));

  // This wraps the generic observer with one whose destructor copies the
  // results into the Module field.
  class BuildIdObserver : public NoteObserver {
   public:
    constexpr explicit BuildIdObserver(typename abi::Abi<Elf>::Module& module)
        : NoteObserver(make_observer(build_id_)), module_(module) {}

    ~BuildIdObserver() {
      if (build_id_) {
        module_.build_id = build_id_->desc;
      }
    }

   private:
    typename abi::Abi<Elf>::Module& module_;
    std::optional<elfldltl::ElfNote> build_id_;
  };

  return BuildIdObserver{module};
}

}  // namespace ld

#endif  // LIB_LD_LOAD_H_
