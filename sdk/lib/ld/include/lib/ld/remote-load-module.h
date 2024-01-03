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
  using TlsDescGot = typename Elf::TlsDescGot;
  using List = fbl::DoublyLinkedList<std::unique_ptr<RemoteLoadModule>>;
  using LoadInfo =
      elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container,
                         elfldltl::PhdrLoadPolicy::kBasic, elfldltl::SegmentWithVmo::NoCopy>;
  using MetadataMemory = elfldltl::LoadInfoMappedMemory<LoadInfo, elfldltl::MappedVmoFile>;
  using Loader = elfldltl::AlignedRemoteVmarLoader;

  // Information from decoding the main executable, specifically.
  struct ExecInfo {
    size_type relative_entry = 0;         // The file-relative entry point address.
    std::optional<size_type> stack_size;  // Requested initial stack size.
  };

  // The decode result of a single module, whether it's an executable or dependency.
  struct DecodeResult {
    std::vector<Soname> needed;  // Names of each DT_NEEDED entry for the module.
    // This information is only relevant for the main executable, and is copied
    // into the DecodeModulesResult that is returned to the caller.
    ExecInfo exec_info;
  };

  // The result returned to the caller after all modules have been decoded.
  struct DecodeModulesResult {
    List modules;        // The list of all decoded modules.
    ExecInfo main_exec;  // Decoded information for the main executable.
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
  std::optional<DecodeResult> Decode(Diagnostics& diag, zx::vmo vmo, uint32_t modid) {
    if (!InitMappedVmo(diag, std::move(vmo))) [[unlikely]] {
      return std::nullopt;
    }

    // Read the file header and program headers into stack buffers.
    auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
        diag, mapped_vmo_, elfldltl::NoArrayFromFile<Phdr>{});
    if (!headers) [[unlikely]] {
      return std::nullopt;
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
      return std::nullopt;
    }

    auto [dyn_phdr, tls_phdr, stack_size] = *result;

    // After successfully decoding the phdrs, we may now instantiate the module
    // and set its fields.
    EmplaceModule(name(), modid);

    module().symbols_visible = true;

    if (build_id) {
      module().build_id = build_id->desc;
    }

    // Fix up segments to be compatible with AlignedRemoteVmarLoader.
    if (!elfldltl::SegmentWithVmo::AlignSegments(diag, load_info(), vmo_.borrow(),
                                                 Loader::page_size())) {
      return std::nullopt;
    }

    auto memory = metadata_memory();
    SetModulePhdrs(module(), ehdr, load_info(), memory);

    auto needed = DecodeDynamic(diag, dyn_phdr);
    if (!needed) {
      return std::nullopt;
    }

    return DecodeResult{
        .needed = std::move(*needed),
        .exec_info =
            {
                .relative_entry = ehdr.entry,
                .stack_size = stack_size,
            },
    };
  }

  // Decode dynamic sections and store the metadata collected from observers.
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

  // Decode the main executable VMO and all its dependencies. The `get_dep_vmo`
  // callback is used to retrieve the VMO for each DT_NEEDED entry; it takes a
  // `string_view` and should return a `zx::vmo`.
  template <class Diagnostics, typename GetDepVmo>
  static std::optional<DecodeModulesResult> DecodeModules(Diagnostics& diag,
                                                          zx::vmo main_executable_vmo,
                                                          GetDepVmo&& get_dep_vmo) {
    // Decode the main executable first and save its decoded information to
    // include in the result returned to the caller.
    auto exec = std::make_unique<RemoteLoadModule>(abi::Abi<>::kExecutableName);
    auto exec_decode_result = exec->Decode(diag, std::move(main_executable_vmo), 0);
    if (!exec_decode_result) [[unlikely]] {
      return std::nullopt;
    }

    // The main executable will always be the first entry of the modules list.
    List modules;
    modules.push_back(std::move(exec));

    auto decode_deps_result =
        DecodeDeps(diag, modules, exec_decode_result->needed, std::forward<GetDepVmo>(get_dep_vmo));
    if (!decode_deps_result) [[unlikely]] {
      return std::nullopt;
    }

    return DecodeModulesResult{
        .modules = std::move(modules),
        .main_exec = exec_decode_result->exec_info,
    };
  }

  // Initialize the the loader and allocate the address region for the module,
  // updating the module's runtime addr fields on success.
  template <class Diagnostics>
  bool Allocate(Diagnostics& diag, const zx::vmar& vmar) {
    loader_ = Loader{vmar};
    if (!loader_.Allocate(diag, load_info())) {
      return false;
    }
    SetModuleVaddrBounds(module(), load_info(), loader_.load_bias());
    return true;
  }

  template <class Diagnostics>
  static bool AllocateModules(Diagnostics& diag, List& modules, zx::unowned_vmar vmar) {
    auto allocate = [&diag, &vmar](auto& module) { return module.Allocate(diag, *vmar); };
    return OnModules(modules, allocate);
  }

  template <class Diagnostics>
  bool Relocate(Diagnostics& diag, const List& modules) {
    auto mutable_memory = elfldltl::LoadInfoMutableMemory{
        diag, load_info(), elfldltl::SegmentWithVmo::GetMutableMemory<LoadInfo>{vmo_.borrow()}};
    if (!mutable_memory.Init()) {
      return false;
    }
    if (!elfldltl::RelocateRelative(diag, mutable_memory, reloc_info(), load_bias())) {
      return false;
    }
    auto tlsdesc_resolver = [&diag](auto&&... args) {
      diag.FormatError("TODO(fxbug.dev/128502): remote TLSDESC not implemented yet");
      return TlsDescGot{};
    };
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, tlsdesc_resolver);
    return elfldltl::RelocateSymbolic(mutable_memory, diag, reloc_info(), symbol_info(),
                                      load_bias(), resolver);
  }

  template <class Diagnostics>
  static bool RelocateModules(Diagnostics& diag, List& modules) {
    auto relocate = [&](auto& module) { return module.Relocate(diag, modules); };
    return OnModules(modules, relocate);
  }

  // Load the module into its allocated vaddr region.
  template <class Diagnostics>
  bool Load(Diagnostics& diag) {
    return loader_.Load(diag, load_info(), vmo_.borrow());
  }

  template <class Diagnostics>
  static bool LoadModules(Diagnostics& diag, List& modules) {
    auto load = [&diag](auto& module) { return module.Load(diag); };
    return OnModules(modules, load);
  }

  // This must be the last method called with the loader. Direct the loader to
  // preserve the load image before it is garbage collected.
  void Commit() { std::move(loader_).Commit(); }

  static void CommitModules(List& modules) {
    std::for_each(modules.begin(), modules.end(), [](auto& module) { module.Commit(); });
  }

 private:
  template <typename T>
  static bool OnModules(List& modules, T&& callback) {
    return std::all_of(modules.begin(), modules.end(), std::forward<T>(callback));
  }

  static void EnqueueDeps(List& modules, const std::vector<Soname>& needed) {
    for (auto soname : needed) {
      if (std::find(modules.begin(), modules.end(), soname) == modules.end()) {
        modules.push_back(std::make_unique<RemoteLoadModule>(soname));
      }
    }
  }

  // Decode every dependency module, enqueuing new dependencies to the modules
  // list to be decoded as well.
  template <class Diagnostics, typename GetDepVmo>
  static bool DecodeDeps(Diagnostics& diag, List& modules, std::vector<Soname>& needed,
                         GetDepVmo&& get_dep_vmo) {
    // Note, this assumes that ModuleList iterators are not invalidated after
    // push_back(), done by `EnqueueDeps`.  This is true of
    // fbl::DoublyLinkedList.  No assumptions are made on the validity of the
    // end() iterator, so it is checked at every iteration.
    uint32_t symbolizer_modid = 0;
    for (auto it = modules.begin(); it != modules.end(); it++) {
      if (it->HasModule()) {
        // Only the main executable should already be decoded before this loop
        // reaches it. Assert here and proceed to EnqueueDeps.
        assert(it == modules.begin());
        assert(symbolizer_modid == 0);
      } else {
        auto vmo = get_dep_vmo(it->name());
        if (!vmo) [[unlikely]] {
          // If the dep is not found, report the missing dependency, and defer
          // to the diagnostics policy on whether to continue processing.
          if (!diag.MissingDependency(it->name().str())) {
            return false;
          }
          continue;
        }
        auto result = it->Decode(diag, std::move(vmo), ++symbolizer_modid);
        if (!result) [[unlikely]] {
          return false;
        }
        needed = result->needed;
      }

      it->EnqueueDeps(modules, needed);
    }
    return true;
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
