// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_LOAD_H_
#define LIB_LD_LOAD_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/phdr.h>
#include <lib/fit/result.h>

#include <tuple>

#include "memory.h"
#include "module.h"

namespace ld {

// Several functions here fill in the passive ABI Module data structure,
// either after or while setting up LoadInfo and Memory objects.

// Shorthand.
template <class Elf = elfldltl::Elf<>>
using AbiModule = typename abi::Abi<Elf>::Module;

// Set the module Phdrs, which is read directly from the load image, as opposed
// to from a buffer read from the file.
template <class Elf = elfldltl::Elf<>, class LoadInfo, class Memory>
constexpr void SetModulePhdrs(AbiModule<Elf>& module, const typename Elf::Ehdr& ehdr,
                              const LoadInfo& load_info, Memory& memory) {
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

// Set the module vaddr bounds, calculated from the runtime load bias.
template <class Elf = elfldltl::Elf<>, class LoadInfo>
constexpr void SetModuleVaddrBounds(AbiModule<Elf>& module, const LoadInfo& load_info,
                                    typename Elf::size_type load_bias) {
  module.link_map.addr = load_bias;
  module.vaddr_start = load_info.vaddr_start() + load_bias;
  module.vaddr_end = module.vaddr_start + load_info.vaddr_size();
}

template <class Elf = elfldltl::Elf<>>
struct ModulePhdrInfo {
  std::optional<typename Elf::Phdr> dyn_phdr;
  std::optional<typename Elf::Phdr> tls_phdr;
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
constexpr std::optional<ModulePhdrInfo<Elf>> DecodeModulePhdrs(
    Diagnostics& diag, cpp20::span<const typename Elf::Phdr> phdrs,
    PhdrObservers&&... phdr_observers) {
  ModulePhdrInfo<Elf> result;
  if (!elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(result.dyn_phdr),
                             elfldltl::PhdrTlsObserver<Elf>(result.tls_phdr),
                             elfldltl::PhdrStackObserver<Elf>(result.stack_size),
                             std::forward<PhdrObservers>(phdr_observers)...)) [[unlikely]] {
    return std::nullopt;
  }
  return result;
}

// This uses elfldltl::DecodeDynamic to fill the Module fields.  Additional
// dynamic section observers can be passed to include in the same scan.
template <class Elf = elfldltl::Elf<>, class Diagnostics, class Memory,
          typename... DynamicObservers>
constexpr fit::result<bool, cpp20::span<const typename Elf::Dyn>> DecodeModuleDynamic(
    AbiModule<Elf>& module, Diagnostics& diag, Memory& memory,
    const std::optional<typename Elf::Phdr>& dyn_phdr, DynamicObservers&&... dynamic_observers) {
  using Dyn = const typename Elf::Dyn;

  if (!dyn_phdr) [[unlikely]] {
    return fit::error{diag.FormatError("no PT_DYNAMIC program header found")};
  }

  const size_t count = dyn_phdr->filesz / sizeof(Dyn);
  if (count <= 1) [[unlikely]] {
    return fit::error{diag.FormatError("PT_DYNAMIC p_filesz ", dyn_phdr->filesz(),
                                       " too small for any ", sizeof(Dyn),
                                       "-byte entries before DT_NULL entry")};
  }

  auto read_dyn = memory.template ReadArray<Dyn>(dyn_phdr->vaddr, count);
  if (!read_dyn) [[unlikely]] {
    return fit::error{
        diag.FormatError("cannot read", count, " entries from PT_DYNAMIC",
                         elfldltl::FileAddress{dyn_phdr->vaddr}),
    };
  }
  cpp20::span<const Dyn> dyn = *read_dyn;

  module.link_map.ld = dyn.data();

  if (!elfldltl::DecodeDynamic(
          diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(module.symbols),
          elfldltl::DynamicInitObserver(module.init), elfldltl::DynamicFiniObserver(module.fini),
          std::forward<DynamicObservers>(dynamic_observers)...)) [[unlikely]] {
    return fit::error{false};
  }

  module.soname = module.symbols.soname();

  return fit::ok(dyn);
}

// Return an observer object to be passed to elfldltl::*NoteObserver.  When
// that object is destroyed, it fills in the Module::build_id field.  This can
// be used either via elfldltl::PhdrFileNoteObserver in DecodeModulePhdrs, or
// via elfldltl::PhdrMemoryNoteObserver in a second elfldltl::DecodePhdrs scan
// performed after the module has been loaded.
template <class Elf = elfldltl::Elf<>>
constexpr auto ObserveBuildIdNote(AbiModule<Elf>& module, bool keep_going = true) {
  // Use the toolkit's generic observer to find the build ID note.
  using ObserverResult = std::optional<elfldltl::ElfNote>&;
  auto make_observer = [keep_going](ObserverResult& build_id) {
    return elfldltl::ObserveBuildIdNote(build_id, keep_going);
  };
  using NoteObserver = decltype(make_observer(std::declval<ObserverResult>()));
  using MakeObserver = decltype(make_observer);

  // This wraps the generic observer with one whose destructor copies the
  // results into the Module field.
  class BuildIdObserver : public NoteObserver {
   public:
    constexpr BuildIdObserver(typename abi::Abi<Elf>::Module& module,
                              const MakeObserver& make_observer)
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

  return BuildIdObserver{module, make_observer};
}

}  // namespace ld

#endif  // LIB_LD_LOAD_H_
