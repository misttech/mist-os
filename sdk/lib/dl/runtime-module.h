// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_RUNTIME_MODULE_H_
#define LIB_DL_RUNTIME_MODULE_H_

#include <lib/elfldltl/alloc-checker-container.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/symbol.h>
#include <lib/ld/load.h>  // For ld::AbiModule

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/vector.h>

namespace dl {

using Elf = elfldltl::Elf<>;
using Soname = elfldltl::Soname<>;

// Forward declaration; defined below.
class RuntimeModule;

// A list of unique "permanent" RuntimeModule data structures used to represent
// a loaded file in the system image.
using ModuleList = fbl::DoublyLinkedList<std::unique_ptr<RuntimeModule>>;

// Use an AllocCheckerContainer that supports fallible allocations; methods
// return a boolean value to signify allocation success or failure.
template <typename T>
using Vector = elfldltl::AllocCheckerContainer<fbl::Vector>::Container<T>;

// TODO(https://fxbug.dev/324136831): comment on how RuntimeModule relates to
// startup modules when the latter is supported.
// TODO(https://fxbug.dev/328135195): comment on the reference counting when
// that gets implemented.

// RuntimeModules are managed by the RuntimeDynamicLinker. A RuntimeModule is
// created for every unique ELF file object loaded either directly or indirectly
// as a dependency of another module. It holds the ld::abi::Abi<...>::Module
// data structure that describes the module in the passive ABI
// (see <sdk/lib/ld/module.h>).

// A RuntimeModule is created along with a LinkingSession::SessionModule (see
// linking-session.h) to represent the ELF file when it is first loaded by
// `dlopen`. Whereas a SessionModule is ephemeral and only lives as long as the
// the LinkingSession in a `dlopen` call, the RuntimeModule is a "permanent"
// data structure that is kept alive in the RuntimeDynamicLinker's `modules_`
// list until it is unloaded.

// While this is an internal API, a RuntimeModule* is the void* handle returned
// by the public <dlfcn.h> API.
class RuntimeModule : public fbl::DoublyLinkedListable<std::unique_ptr<RuntimeModule>> {
 public:
  using Addr = Elf::Addr;
  using SymbolInfo = elfldltl::SymbolInfo<Elf>;
  using AbiModule = ld::AbiModule<>;

  // Not copyable, but movable.
  RuntimeModule(const RuntimeModule&) = delete;
  RuntimeModule(RuntimeModule&&) = default;

  // See unmap-[posix|zircon].cc for the dtor. On destruction, the module's load
  // image is unmapped per the semantics of the OS implementation.
  ~RuntimeModule();

  // The name of the runtime module is set to the filename passed to dlopen() to
  // create the runtime module. This is usually the same as the DT_SONAME of the
  // AbiModule, but that is not guaranteed. When performing an equality check,
  // match against both possible name values.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || name == abi_module_.soname;
  }

  constexpr const Soname& name() const { return name_; }

  // TODO(https://fxbug.dev/333920495): pass in the symbolizer_modid.
  [[nodiscard]] static std::unique_ptr<RuntimeModule> Create(fbl::AllocChecker& ac, Soname name) {
    std::unique_ptr<RuntimeModule> module{new (ac) RuntimeModule};
    if (module) [[likely]] {
      module->name_ = name;
    }
    return module;
  }

  constexpr AbiModule& module() { return abi_module_; }

  constexpr const AbiModule& module() const { return abi_module_; }

  constexpr Addr load_bias() const { return abi_module_.link_map.addr; }

  const SymbolInfo& symbol_info() const { return abi_module_.symbols; }

  size_t vaddr_size() const { return abi_module_.vaddr_end - abi_module_.vaddr_start; }

 private:
  // A RuntimeModule can only be created with Module::Create...).
  RuntimeModule() = default;

  static void Unmap(uintptr_t vaddr, size_t len);

  Soname name_;
  AbiModule abi_module_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_MODULE_H_
