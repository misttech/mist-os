// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_RUNTIME_MODULE_H_
#define LIB_DL_RUNTIME_MODULE_H_

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

namespace dl {

using Elf = elfldltl::Elf<>;
using Soname = elfldltl::Soname<>;

class ModuleHandle;
// A list of unique "permanent" ModuleHandle data structures used to represent
// a loaded file in the system image.
using ModuleHandleList = fbl::DoublyLinkedList<std::unique_ptr<ModuleHandle>>;

// Use an AllocCheckerContainer that supports fallible allocations; methods
// return a boolean value to signify allocation success or failure.
template <typename T>
using Vector = elfldltl::AllocCheckerContainer<fbl::Vector>::Container<T>;

// TODO(https://fxbug.dev/324136831): comment on how ModuleHandle relates to
// startup modules when the latter is supported.
// TODO(https://fxbug.dev/328135195): comment on the reference counting when
// that gets implemented.

// A ModuleHandle is created for every unique ELF file object loaded either
// directly or indirectly as a dependency of another module. It holds the
// ld::abi::Abi<...>::Module data structure that describes the module in the
// passive ABI (see //sdk/lib/ld/module.h).

// A ModuleHandle has a corresponding LoadModule (see below) to represent the
// ELF file when it is first loaded by `dlopen`. Whereas a LoadModule is
// ephemeral and lives only as long as it takes to load a module and its
// dependencies in `dlopen`, the ModuleHandle is a "permanent" data structure
// that is kept alive in the RuntimeDynamicLinker's `modules_` list until the
// module is unloaded.

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

  // See unmap-[posix|zircon].cc for the dtor. On destruction, the module's load
  // image is unmapped per the semantics of the OS implementation.
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
  [[nodiscard]] static std::unique_ptr<ModuleHandle> Create(fbl::AllocChecker& ac, Soname name) {
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

  size_t vaddr_size() const { return abi_module_.vaddr_end - abi_module_.vaddr_start; }

 private:
  // A ModuleHandle can only be created with Module::Create...).
  ModuleHandle() = default;

  static void Unmap(uintptr_t vaddr, size_t len);
  Soname name_;
  AbiModule abi_module_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_MODULE_H_
