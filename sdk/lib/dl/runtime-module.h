// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_RUNTIME_MODULE_H_
#define LIB_DL_RUNTIME_MODULE_H_

// Avoid symbol conflict between <ld/abi/abi.h> and <link.h>
#pragma push_macro("_r_debug")
#undef _r_debug
#define _r_debug not_using_system_r_debug
#include <link.h>
#pragma pop_macro("_r_debug")

#include <lib/elfldltl/alloc-checker-container.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/symbol.h>
#include <lib/ld/abi.h>
#include <lib/ld/load.h>  // For ld::AbiModule
#include <lib/ld/tls.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/vector.h>

#include "diagnostics.h"

namespace dl {

using AbiModule = ld::AbiModule<>;
using Elf = elfldltl::Elf<>;
using Soname = elfldltl::Soname<>;

// Forward declaration; defined below.
class RuntimeModule;

// A list of unique "permanent" RuntimeModule data structures used to represent
// a loaded file in the system image.
using ModuleList = fbl::DoublyLinkedList<std::unique_ptr<RuntimeModule>, fbl::DefaultObjectTag,
                                         fbl::SizeOrder::Constant>;

// Use an AllocCheckerContainer that supports fallible allocations; methods
// return a boolean value to signify allocation success or failure.
template <typename T>
using Vector = elfldltl::AllocCheckerContainer<fbl::Vector>::Container<T>;

// A list of valid non-owning references to RuntimeModules.
using ModuleRefList = Vector<const RuntimeModule*>;

// These are helpers to allow specific ways to access an element.
struct DerefElement {
  constexpr decltype(auto) operator()(auto&& ptr) const { return *ptr; }
};

struct AsConstElement {
  constexpr decltype(auto) operator()(const auto& elt) const { return elt; }
};

constexpr auto DerefElementsView = std::views::transform(DerefElement());
constexpr auto AsConstElementsView = std::views::transform(AsConstElement());

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
  using TlsModule = ld::abi::Abi<>::TlsModule;
  using size_type = Elf::size_type;

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

  // This is called if the RuntimeModule is created from a startup module (i.e.
  // a module that was linked and loaded with the running program). This sets
  // the abi and TLS information onto the RuntimeModule.
  void SetStartupModule(const AbiModule& abi_module, const ld::abi::Abi<>& abi) {
    abi_module_ = abi_module;
    can_unload_ = false;
    initialized_ = true;

    size_t tls_modid = abi_module_.tls_modid;
    if (tls_modid > 0) {
      const size_t idx = tls_modid - 1;
      tls_module_ = abi.static_tls_modules[idx];
      static_tls_bias_ = abi.static_tls_offsets[idx];
    }
  }

  constexpr AbiModule& module() { return abi_module_; }
  constexpr const AbiModule& module() const { return abi_module_; }

  constexpr const TlsModule& tls_module() const { return tls_module_; }

  size_t vaddr_size() const { return abi_module_.vaddr_end - abi_module_.vaddr_start; }

  // The following methods satisfy the Module template API for use with
  // elfldltl::ResolverDefinition (see <lib/elfldltl/resolve.h>).

  const SymbolInfo& symbol_info() const { return abi_module_.symbols; }

  constexpr Addr load_bias() const { return abi_module_.link_map.addr; }

  constexpr size_type tls_module_id() const { return abi_module_.tls_modid; }

  constexpr bool uses_static_tls() const { return ld::ModuleUsesStaticTls(abi_module_); }

  constexpr size_t static_tls_bias() const { return static_tls_bias_; }

  constexpr fit::result<bool, const typename Elf::Sym*> Lookup(  //
      Diagnostics& diag, elfldltl::SymbolName& name) const {
    return fit::ok(name.Lookup(symbol_info()));
  }

  // Return a view of `list` with all of its elements dereferenced and made
  // constant (i.e. Vector<const RuntimeModule*> -> View<const RuntimeModule&>).
  static constexpr auto const_derefed_element_view(const ModuleRefList& list) {
    return AsConstElementsView(DerefElementsView(list));
  }

  // This is a list of module pointers to this module's DT_NEEDEDs, i.e. the
  // first level of dependencies in this module's module tree. If this list is
  // empty, the module does not have any dependencies.
  constexpr ModuleRefList& direct_deps() { return direct_deps_; }
  // Return a view of const references to each direct_deps module.
  constexpr auto GetDirectDeps() const { return const_derefed_element_view(direct_deps_); }

  // This is the breadth-first ordered list of module references, representing
  // this module's tree of modules. A reference to this module (the root) is
  // always the first in this list. The other modules in this list are
  // non-owning references to modules that were explicitly linked with this
  // module; global modules that may have been used for relocations, but are not
  // a DT_NEEDED of any dependency, are not included in this list.
  // This list is set when dlopen() is called on this module.
  constexpr auto module_tree() const {
    // RuntimeModule::ReifyModuleTree should have ben called before any callers
    // call this accessor.
    assert(!module_tree_.is_empty());
    return const_derefed_element_view(module_tree_);
  }

  // Constructs this module's `module_tree` if it has not been set yet.
  bool ReifyModuleTree(Diagnostics& diag);

  // Whether this module is a global module: either the module was loaded at
  // startup or loaded by dlopen() with the RTLD_GLOBAL flag.
  constexpr bool is_global() const { return abi_module_.symbols_visible; }
  constexpr void set_global() { abi_module_.symbols_visible = true; }
  constexpr bool is_local() const { return !is_global(); }

  // Run the init functions for this module (as the root module) and the init
  // functions for all its dependencies.
  void InitializeModuleTree();

  // Run only this module's init functions. This is called if this module is
  // loaded as a dependency to another module.
  void Initialize();

  // Construct the `dl_phdr_info` for this module.
  dl_phdr_info MakePhdrInfo(uint64_t global_loaded, uint64_t global_unloaded) const;

 private:
  // A RuntimeModule can only be created with Module::Create...).
  RuntimeModule() = default;

  static void Unmap(uintptr_t vaddr, size_t len);

  Soname name_;
  AbiModule abi_module_;
  TlsModule tls_module_;
  size_type static_tls_bias_ = 0;
  bool can_unload_ = true;
  ModuleRefList direct_deps_;
  ModuleRefList module_tree_;
  bool initialized_ = false;
};

// This is the module tree view type returned by RuntimeModule::module_tree.
using ModuleTree = decltype(std::declval<RuntimeModule>().module_tree());

}  // namespace dl

#endif  // LIB_DL_RUNTIME_MODULE_H_
