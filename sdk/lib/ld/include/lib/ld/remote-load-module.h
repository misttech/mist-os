// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_LOAD_MODULE_H_
#define LIB_LD_REMOTE_LOAD_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/loadinfo-mapped-memory.h>
#include <lib/elfldltl/loadinfo-mutable-memory.h>
#include <lib/elfldltl/mapped-vmo-file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/segment-with-vmo.h>
#include <lib/elfldltl/soname.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>

#include <algorithm>

#include <fbl/intrusive_double_list.h>

namespace ld {

// RemoteLoadModule is the LoadModule type used in the remote dynamic linker.
using RemoteLoadModuleBase =
    LoadModule<elfldltl::Elf<>, elfldltl::StdContainer<std::vector>::Container,
               LoadModuleInline::kYes, LoadModuleRelocInfo::kYes, elfldltl::SegmentWithVmo::NoCopy>;

template <class Elf = elfldltl::Elf<>>
struct RemoteLoadModule : public RemoteLoadModuleBase,
                          fbl::DoublyLinkedListable<std::unique_ptr<RemoteLoadModule<Elf>>> {
 public:
  using typename RemoteLoadModuleBase::Phdr;
  using typename RemoteLoadModuleBase::size_type;
  using typename RemoteLoadModuleBase::Soname;
  using Ehdr = typename Elf::Ehdr;
  using List = fbl::DoublyLinkedList<std::unique_ptr<RemoteLoadModule>>;
  using LoadInfo =
      elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container,
                         elfldltl::PhdrLoadPolicy::kBasic, elfldltl::SegmentWithVmo::NoCopy>;
  using MetadataMemory = elfldltl::LoadInfoMappedMemory<LoadInfo, elfldltl::MappedVmoFile>;
  using Loader = elfldltl::AlignedRemoteVmarLoader;

  struct DecodeResult {
    std::vector<Soname> needed;  // Names of each DT_NEEDED entry for the module.

    // These are only of interest for the main executable.
    size_type relative_entry = 0;         // The file-relative entry point address.
    std::optional<size_type> stack_size;  // Requested initial stack size.
  };

  RemoteLoadModule() = delete;

  RemoteLoadModule(RemoteLoadModule&&) = delete;

  explicit RemoteLoadModule(const Soname& name) : RemoteLoadModuleBase{name} {}

  // Initialize the module from the provided VMO, representing either the
  // binary or shared library to be loaded. Create the data structures that make
  // make the VMO readable, and scan and decode its phdrs to set and return
  // relevant information about the module to make it ready for relocation and
  // loading. Return a `DecodeResult` containing information about this
  // module's dependencies.
  template <class Diagnostics>
  DecodeResult Decode(Diagnostics& diag, zx::vmo vmo) {
    if (!InitMappedVmo(diag, std::move(vmo))) [[unlikely]] {
      return {};
    }

    // Read the file header and program headers into stack buffers.
    auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
        diag, mapped_vmo_, elfldltl::NoArrayFromFile<Phdr>{});
    if (!headers) [[unlikely]] {
      return {};
    }

    // Decode phdrs to fill LoadInfo, BuildId, etc.
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const Ehdr& ehdr = ehdr_owner;
    const cpp20::span<const Phdr> phdrs = phdrs_owner;
    std::optional<elfldltl::ElfNote> build_id;
    auto result =
        DecodeModulePhdrs(diag, phdrs, load_info().GetPhdrObserver(Loader::page_size()),
                          elfldltl::PhdrFileNoteObserver(
                              elfldltl::Elf<>{}, mapped_vmo_, elfldltl::NoArrayFromFile<Phdr>{},
                              elfldltl::ObserveBuildIdNote(build_id, true)));
    if (!result) [[unlikely]] {
      return {};
    }

    auto [dyn_phdr, tls_phdr, stack_size] = *result;

    // After successfully decoding the phdrs, we may now instantiate the module
    // and set its fields.
    EmplaceModule(name());

    module().symbols_visible = true;

    if (build_id) {
      module().build_id = build_id->desc;
    }

    auto memory = metadata_memory();
    SetModulePhdrs(module(), ehdr, load_info(), memory);

    auto needed = DecodeDynamic(diag, dyn_phdr);
    if (!needed) {
      return {};
    }

    return DecodeResult{
        .needed = std::move(*needed), .relative_entry = ehdr.entry, .stack_size = stack_size};
  }

 private:
  // Decode dynamic sections and store the metadata scanned by observers.
  // A vector of each DT_NEEDED entry string name is returned.
  template <class Diagnostics>
  std::optional<std::vector<Soname>> DecodeDynamic(
      Diagnostics& diag, const std::optional<typename Elf::Phdr>& dyn_phdr) {
    static const constexpr std::string_view kCollectionError = "Failed to push value to container.";
    using NeededObserver = elfldltl::DynamicValueCollectionObserver<
        Elf, elfldltl::ElfDynTag::kNeeded,
        elfldltl::StdContainer<std::vector>::Container<size_type>, kCollectionError>;

    // It is not guaranteed that the symbol table has been scanned before every
    // DT_NEEDED entry, so create an observer to collect their offsets so that
    // the string name can be accessed after `DecodeModuleDynamic` returns.
    elfldltl::StdContainer<std::vector>::Container<size_type> needed_strtab_offsets;

    auto memory = metadata_memory();
    auto result =
        DecodeModuleDynamic(module(), diag, memory, dyn_phdr, NeededObserver(needed_strtab_offsets),
                            elfldltl::DynamicRelocationInfoObserver(reloc_info()));
    if (result.empty()) [[unlikely]] {
      return std::nullopt;
    }

    // Now that the symbol table has been decoded, we may access the names of
    // each offset entry.
    auto ok = true;
    std::vector<Soname> needed_names;
    needed_names.reserve(needed_strtab_offsets.size());
    std::transform(needed_strtab_offsets.begin(), needed_strtab_offsets.end(),
                   std::back_inserter(needed_names), [this, &diag, &ok](size_type offset) {
                     std::string_view name = symbol_info().string(offset);
                     if (name.empty()) {
                       ok = diag.FormatError("Invalid offset ", offset, " in DT_NEEDED entry");
                     }
                     return Soname{name};
                   });
    if (!ok) [[unlikely]] {
      return std::nullopt;
    }

    return needed_names;
  }

  // Create and return a memory-adaptor object that serves as a wrapper
  // around this module's LoadInfo and MappedVmoFile. This is used to
  // translate vaddrs into file-relative offsets in order to read from the VMO.
  MetadataMemory metadata_memory() { return MetadataMemory{load_info(), mapped_vmo_}; }

  template <class Diagnostics>
  bool InitMappedVmo(Diagnostics& diag, zx::vmo vmo) {
    if (auto status = mapped_vmo_.Init(vmo.borrow()); status.is_error()) {
      diag.SystemError("cannot map VMO file for ", name(), " : ",
                       elfldltl::ZirconError{status.status_value()});
      return false;
    }
    vmo_ = std::move(vmo);
    return true;
  }

  Loader loader_;
  elfldltl::MappedVmoFile mapped_vmo_;
  zx::vmo vmo_;
};

}  // namespace ld

#endif  // LIB_LD_REMOTE_LOAD_MODULE_H_
