// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_LINKING_SESSION_H_
#define LIB_DL_LINKING_SESSION_H_

#include <lib/elfldltl/resolve.h>
#include <lib/fit/result.h>
#include <lib/ld/decoded-module-in-memory.h>
#include <lib/ld/load-module.h>

#include "diagnostics.h"
#include "runtime-module.h"

namespace dl {

// LoadModule is the temporary data structure created to load a file; a
// LoadModule is created when a file needs to be loaded, and is destroyed after
// the file module and all its dependencies have been loaded, decoded, symbols
// resolved, and relro protected.
template <class Loader>
class LoadModule;

// A list of unique "temporary" LoadModule data structures used for loading a
// file.
template <class Loader>
using LoadModuleList = fbl::DoublyLinkedList<std::unique_ptr<LoadModule<Loader>>>;

template <class Loader>
class LoadModule : public ld::LoadModule<ld::DecodedModuleInMemory<>>,
                   public fbl::DoublyLinkedListable<std::unique_ptr<LoadModule<Loader>>> {
 public:
  using Relro = typename Loader::Relro;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<ld::kMaxSegments>::Container>;
  using TlsDescGot = Elf::TlsDescGot;

  // TODO(https://fxbug.dev/331421403): Implement TLS.
  struct NoTlsDesc {
    constexpr TlsDescGot operator()() const {
      assert(false && "TLS is not supported");
      return {};
    }
    template <class Diagnostics, class Definition>
    constexpr fit::result<bool, TlsDescGot> operator()(Diagnostics& diag,
                                                       const Definition& defn) const {
      assert(false && "TLS is not supported");
      return fit::error{false};
    }
  };

  // This is the observer used to collect DT_NEEDED offsets from the dynamic phdr.
  static const constexpr std::string_view kNeededError{"DT_NEEDED offsets"};
  using NeededObserver = elfldltl::DynamicValueCollectionObserver<  //
      Elf, elfldltl::ElfDynTag::kNeeded, Vector<size_type>, kNeededError>;

  // The LoadModule::Create(...) takes a reference to the ModuleHandle for the
  // file, setting information on it during the loading, decoding, and
  // relocation process.
  [[nodiscard]] static std::unique_ptr<LoadModule> Create(fbl::AllocChecker& ac,
                                                          ModuleHandle& module) {
    std::unique_ptr<LoadModule> load_module{new (ac) LoadModule(module)};
    if (load_module) [[likely]] {
      load_module->set_name(module.name());
      // Have the underlying DecodedModule (see <lib/ld/decoded-module.h>) point to
      // the ABIModule embedded in the ModuleHandle, so that its information will
      // be filled out during decoding operations.
      load_module->decoded().set_module(module.module());
    }
    return load_module;
  }

  // Load `file` into the system image, decode phdrs and save the metadata in
  // the the ABI module. Decode the module's dependencies (if any), and
  // return a vector their so names.
  template <class File>
  std::optional<Vector<Soname>> Load(Diagnostics& diag, File&& file) {
    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.
    Loader loader;
    auto headers = decoded().LoadFromFile(diag, loader, std::forward<File>(file));
    if (!headers) [[unlikely]] {
      return std::nullopt;
    }

    Vector<size_type> needed_offsets;
    // TODO(https://fxbug.dev/331421403): TLS is not supported yet.
    size_type max_tls_modid = 0;
    if (!decoded().DecodeFromMemory(  //
            diag, loader.memory(), loader.page_size(), *headers, max_tls_modid,
            elfldltl::DynamicRelocationInfoObserver(decoded().reloc_info()),
            NeededObserver(needed_offsets))) [[unlikely]] {
      return std::nullopt;
    }

    // After successfully loading the file, finalize the module's mapping by
    // calling `Commit` on the loader. Save the returned relro capability that
    // will be used to apply relro protections later.
    relro_ = decoded().CommitLoader(std::move(loader));

    // TODO(https://fxbug.dev/324136435): The code that parses the names from
    // the symbol table be shared with <lib/ld/remote-decoded-module.h>.
    if (Vector<Soname> needed_names;
        needed_names.reserve(diag, kNeededError, needed_offsets.size())) [[likely]] {
      for (size_type offset : needed_offsets) {
        std::string_view name = this->symbol_info().string(offset);
        if (name.empty()) [[unlikely]] {
          diag.FormatError("DT_NEEDED has DT_STRTAB offset ", offset, " with DT_STRSZ ",
                           this->symbol_info().strtab().size());
          return std::nullopt;
        }
        if (!needed_names.push_back(diag, kNeededError, Soname{name})) [[unlikely]] {
          return std::nullopt;
        }
      }
      return std::move(needed_names);
    }

    return std::nullopt;
  }

  // Perform relative and symbolic relocations, resolving symbols from the
  // list of modules as needed.
  bool Relocate(Diagnostics& diag, LoadModuleList<Loader>& modules) {
    constexpr NoTlsDesc kNoTlsDesc{};
    auto memory = ld::ModuleMemory{module()};
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, kNoTlsDesc);
    return elfldltl::RelocateRelative(diag, memory, reloc_info(), load_bias()) &&
           elfldltl::RelocateSymbolic(memory, diag, reloc_info(), symbol_info(), load_bias(),
                                      resolver);
  }

  // Apply relro protections. `relro_` cannot be used after this call.
  bool ProtectRelro(Diagnostics& diag) { return std::move(relro_).Commit(diag); }

 private:
  // A LoadModule can only be created with LoadModule::Create...).
  explicit LoadModule(ModuleHandle& module) : module_(module) {}

  // This is a reference to the "permanent" module data structure that this
  // LoadModule is responsible for: runtime information is set on the `module_`
  // during the course of the loading process. Whereas this LoadModule instance
  // will get destroyed at the end of `dlopen`, its `module_` will live as long
  // as the file is loaded in the RuntimeDynamicLinker's `modules_` list.
  ModuleHandle& module_;
  // The relro capability that is provided when the module is decoded and is
  // used to apply relro protections after the module is relocated.
  Relro relro_;
};

}  // namespace dl

#endif  // LIB_DL_LINKING_SESSION_H_
