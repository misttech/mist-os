// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/alloc-checker-container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/fit/result.h>
#include <lib/ld/decoded-module-in-memory.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>
#include <lib/ld/memory.h>
#include <lib/ld/module.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/vector.h>

#include "diagnostics.h"

namespace dl {

using Elf = elfldltl::Elf<>;
using Soname = elfldltl::Soname<>;

// TODO(https://fxbug.dev/324136435): These KMax* variables are copied from
// //sdk/lib/ld/startup-load.h, we should share these symbols instead.
// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

template <class ModuleType>
using ModuleList = fbl::DoublyLinkedList<std::unique_ptr<ModuleType>>;

// TODO(https://fxbug.dev/324136831): comment on how ModuleHandle relates to
// startup modules when the latter is supported.
// TODO(https://fxbug.dev/328135195): comment on the reference counting when
// that gets implemented.

// A ModuleHandle is created for every unique ELF file object loaded either
// directly or indirectly as a dependency of another module. It holds the
// ld::abi::Abi<...>::Module data structure that describes the module in the
// passive ABI (see //sdk/lib/ld/module.h).

// A ModuleHandle is created by its corresponding LoadModule (see below) when
// an ELF file is first loaded by `dlopen`. Whereas a LoadModule is ephemeral
// and lives only as long as it takes to load a module and its dependencies in
// `dlopen`, the ModuleHandle is a "permanent" data structure that is kept alive
// in the RuntimeDynamicLinker's `loaded_modules` list until the module is
// unloaded.

// While this is an internal API, a ModuleHandle* is the void* handle returned
// by the public <dlfcn.h> API.
class ModuleHandle : public fbl::DoublyLinkedListable<std::unique_ptr<ModuleHandle>> {
 public:
  using Addr = Elf::Addr;
  using SymbolInfo = elfldltl::SymbolInfo<Elf>;
  using AbiModule = ld::AbiModule<>;

  // Not copyable, but movable.
  ModuleHandle(const ModuleHandle&) = delete;
  ModuleHandle(ModuleHandle&&) = default;

  ~ModuleHandle();

  // The name of the module handle is set to the filename passed to dlopen() to
  // create the module handle. This is usually the same as the DT_SONAME of the
  // AbiModule, but that is not guaranteed. When performing an equality check,
  // match against both possible name values.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || name == abi_module_.soname;
  }

  constexpr const Soname& name() const { return name_; }

  // TODO(https://fxbug.dev/333920495): pass in the symbolizer_modid.
  [[nodiscard]] static std::unique_ptr<ModuleHandle> Create(Soname name, fbl::AllocChecker& ac) {
    std::unique_ptr<ModuleHandle> module{new (ac) ModuleHandle};
    if (module) [[likely]] {
      module->name_ = name;
    }
    return module;
  }

  constexpr AbiModule& module() { return abi_module_; }

  constexpr const AbiModule& module() const { return abi_module_; }

  constexpr Addr load_bias() const { return abi_module_.link_map.addr; }

  const SymbolInfo& symbol_info() const { return abi_module_.symbols; }

 private:
  // A ModuleHandle can only be created with Module::Create...).
  ModuleHandle() = default;

  static void Unmap(uintptr_t vaddr, size_t len);
  Soname name_;
  AbiModule abi_module_;
};

// Use a AllocCheckerContainer that supports fallible allocations; methods return
// a boolean value to signify allocation success or failure.
template <typename T>
using Vector = elfldltl::AllocCheckerContainer<fbl::Vector>::Container<T>;

// LoadModule is the temporary data structure created to load a file; a
// LoadModule is created when a file needs to be loaded, and is destroyed after
// the file module and all its dependencies have been loaded, decoded, symbols
// resolved, and relro protected.
template <class OSImpl>
class LoadModule : public ld::LoadModule<ld::DecodedModuleInMemory<>>,
                   public fbl::DoublyLinkedListable<std::unique_ptr<LoadModule<OSImpl>>> {
 public:
  using Loader = typename OSImpl::Loader;
  using File = typename OSImpl::File;
  using Relro = typename Loader::Relro;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<kMaxSegments>::Container>;

  // TODO(https://fxbug.dev/331421403): Implement TLS.
  struct NoTlsDesc {
    using TlsDescGot = typename Elf::TlsDescGot;
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

  // The LoadModule::Create(...) creates and takes temporary ownership of the
  // ModuleHandle for the file in order to set information on the data structure
  // during the loading process. When the loading process has completed,
  // `take_module()` should  be called on the load module before it's destroyed
  // to transfer ownership of the module handle to the caller. Otherwise, if an
  // error occurs during loading, the load module will clean up the module
  // handle in its own destruction.
  // A fbl::AllocChecker& caller_ac is passed in to require the caller to check
  // for allocation success/failure. This function will arm the caller_ac with
  // the allocation results of its local calls.
  [[nodiscard]] static std::unique_ptr<LoadModule> Create(Soname name,
                                                          fbl::AllocChecker& caller_ac) {
    fbl::AllocChecker ac;
    auto module = ModuleHandle::Create(name, ac);
    if (!ac.check()) [[unlikely]] {
      caller_ac.arm(sizeof(ModuleHandle), false);
      return nullptr;
    }
    std::unique_ptr<LoadModule> load_module{new (ac) LoadModule};
    if (!ac.check()) [[unlikely]] {
      caller_ac.arm(sizeof(LoadModule), false);
      return nullptr;
    }
    // TODO(https://fxbug.dev/335921712): Have ModuleHandle own the name string.
    load_module->set_name(name);
    // Have the underlying DecodedModule (see <lib/ld/decoded-module.h>) point to
    // the ABIModule embedded in the ModuleHandle, so that its information will
    // be filled out during decoding operations.
    load_module->decoded().set_module(module->module());
    load_module->module_ = std::move(module);

    // Signal to the caller all allocations have succeeded.
    caller_ac.arm(sizeof(LoadModule), true);
    return std::move(load_module);
  }

  // This must be the last method called on LoadModule and can only be called
  // with `std::move(load_module).take_module();`.
  // Calling this method indicates that the module has been loaded successfully
  // and will give the caller ownership of the module, handing off the
  // responsibility for managing its lifetime and unmapping.
  std::unique_ptr<ModuleHandle> take_module() && { return std::move(module_); }

  // Retrieve the file and load it into the system image, then decode the phdrs
  // to metadata to attach to the ABI module and store the information needed
  // for dependency parsing. Decode the module's dependencies (if any), and
  // return a vector their so names.
  std::optional<Vector<Soname>> Load(Diagnostics& diag) {
    auto file = OSImpl::RetrieveFile(diag, module_->name().str());
    if (!file) [[unlikely]] {
      return std::nullopt;
    }

    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.
    Loader loader;
    auto headers = decoded().LoadFromFile(diag, loader, *std::move(file));
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
    // calling `Commit` on the loader. Save the returned relro capability
    // that will be used to apply relro protections later.
    loader_relro_ = std::move(loader).Commit(decoded().relro_bounds());

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
  bool Relocate(Diagnostics& diag, ModuleList<LoadModule<OSImpl>>& modules) {
    constexpr NoTlsDesc kNoTlsDesc{};
    auto memory = ld::ModuleMemory{module()};
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, kNoTlsDesc);
    return elfldltl::RelocateRelative(diag, memory, reloc_info(), load_bias()) &&
           elfldltl::RelocateSymbolic(memory, diag, reloc_info(), symbol_info(), load_bias(),
                                      resolver);
  }

 private:
  // A LoadModule can only be created with LoadModule::Create...).
  LoadModule() = default;

  std::unique_ptr<ModuleHandle> module_;
  Relro loader_relro_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
