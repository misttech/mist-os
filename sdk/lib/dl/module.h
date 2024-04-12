// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/fit/result.h>
#include <lib/ld/decoded-module-in-memory.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>
#include <lib/ld/module.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>

#include "diagnostics.h"

namespace dl {

using Elf = elfldltl::Elf<>;

// TODO(https://fxbug.dev/324136435): These KMax* variables are copied from
// //sdk/lib/ld/startup-load.h, we should share these symbols instead.
// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

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
  using Soname = elfldltl::Soname<>;
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

using LoadModuleBase = ld::LoadModule<ld::DecodedModuleInMemory<>>;

// TODO(https://fxbug.dev/324136435): Replace the code and functions copied from
// ld::LoadModule to use them directly; then continue to use methods directly
// from the //sdk/lib/ld public API, refactoring them into shared code as needed.

// LoadModule is the temporary data structure created to load a file; a
// LoadModule is created when a file needs to be loaded, and is destroyed after
// the file module and all its dependencies have been loaded, decoded, symbols
// resolved, and relro protected.
template <class OSImpl>
class LoadModule : public LoadModuleBase,
                   public fbl::DoublyLinkedListable<std::unique_ptr<LoadModule<OSImpl>>> {
 public:
  using Loader = typename OSImpl::Loader;
  using File = typename OSImpl::File;
  using Relro = typename Loader::Relro;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using Soname = elfldltl::Soname<>;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<kMaxSegments>::Container>;

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
      caller_ac.arm(sizeof(ModuleHandle), false);
      return nullptr;
    }
    load_module->set_name(name);
    // Have the underlying DecodedModule (see <lib/ld/decoded-module.h>) point to
    // the ABIModule embedded in the ModuleHandle, so that its information will
    // be filled out during decoding operations.
    load_module->decoded().set_module(module->module());
    load_module->module_ = std::move(module);

    // Signal to the caller all allocations have succeeded.
    caller_ac.arm(sizeof(ModuleHandle), true);
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
  // for dependency parsing.
  bool Load(Diagnostics& diag) {
    auto file = OSImpl::RetrieveFile(diag, module_->name().str());
    if (!file) [[unlikely]] {
      return false;
    }

    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.
    Loader loader;
    auto headers = decoded().LoadFromFile(diag, loader, *std::move(file));
    if (!headers) [[unlikely]] {
      return false;
    }
    auto& [ehdr_owner, phdrs_owner] = *headers;
    cpp20::span<const Phdr> phdrs = phdrs_owner;

    // After successfully loading the file, finalize the module's mapping by
    // calling `Commit` on the loader. Save the returned relro capability
    // that will be used to apply relro protections later.
    // TODO(https://fxbug.dev/323418587): For now, pass an empty relro_bounds.
    // This will eventually take the decoded relro_phdr.
    loader_relro_ = std::move(loader).Commit(LoadInfo::Region{});

    // Now that the file is in memory, we can decode the phdrs.
    std::optional<Phdr> dyn_phdr;
    if (!elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(dyn_phdr))) {
      return false;
    }

    // TODO(caslyn): Use ld::DecodeFromMemory with a
    // elfldltl::DynamicValueCollectionObserver so that we collect the needed
    // entries into an fbl::Vector<size_type> in one pass. Have this Load
    // function return the std::optional result of that to pass to EnqueueDeps.
    // From the dynamic phdr, decode the metadata and dependency information.
    auto memory = ld::ModuleMemory{decoded().module()};
    auto dyn = DecodeModuleDynamic(
        decoded().module(), diag, memory, dyn_phdr,
        elfldltl::DynamicRelocationInfoObserver(decoded().reloc_info()),
        elfldltl::DynamicTagCountObserver<Elf, elfldltl::ElfDynTag::kNeeded>(needed_count_));
    if (dyn.is_error()) [[unlikely]] {
      return {};
    }

    dynamic_ = dyn.value();

    return true;
  }

  // TODO(caslyn): This will become a method on the RuntimeDynamicLinker.
  // It also needs to handle if the module is already loaded.
  bool EnqueueDeps(Diagnostics& diag, fbl::DoublyLinkedList<std::unique_ptr<LoadModule>>& modules) {
    auto needed_count = needed_count_;

    auto handle_needed = [&](std::string_view soname_str) {
      assert(needed_count > 0);
      Soname soname{soname_str};

      // Check if this dependency was already added to the modules list.
      if (std::find(modules.begin(), modules.end(), soname) != modules.end()) {
        return true;
      }

      fbl::AllocChecker ac;
      auto load_module = LoadModule<OSImpl>::Create(soname, ac);
      if (!ac.check()) [[unlikely]] {
        // TODO(caslyn): communicate out of memory error.
        return false;
      }

      // TOOD(https://fxrev.dev/323419430): Add new module to current module's dependency list.

      modules.push_back(std::move(load_module));

      return --needed_count > 0;
    };

    auto observer = elfldltl::DynamicNeededObserver(symbol_info(), handle_needed);
    auto memory = ld::ModuleMemory{decoded().module()};
    elfldltl::DecodeDynamic(diag, memory, dynamic_, observer);
    return true;
  }

 private:
  // A LoadModule can only be created with LoadModule::Create...).
  LoadModule() = default;

  std::unique_ptr<ModuleHandle> module_;
  Relro loader_relro_;
  // TODO(caslyn): These are stored here for now, for convenience. After
  // dependency enqueueing and sharing code with //sdk/lib/ld, we will be able
  // to see more clearly how best to pass these values around.
  cpp20::span<const Dyn> dynamic_;
  size_t needed_count_ = 0;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
