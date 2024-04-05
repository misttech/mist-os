// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
#define LIB_DL_RUNTIME_DYNAMIC_LINKER_H_

#include <dlfcn.h>  // for RTLD_* macros
#include <lib/elfldltl/soname.h>
#include <lib/fit/result.h>

#include <fbl/intrusive_double_list.h>

#include "diagnostics.h"
#include "error.h"
#include "module.h"

namespace dl {

enum OpenSymbolScope : int {
  kLocal = RTLD_LOCAL,
  kGlobal = RTLD_GLOBAL,
};

enum OpenBindingMode : int {
  kNow = RTLD_NOW,
  // RTLD_LAZY functionality is not supported, but keep the flag definition
  // because it's a legitimate flag that can be passed in.
  kLazy = RTLD_LAZY,
};

enum OpenFlags : int {
  kNoload = RTLD_NOLOAD,
  kNodelete = RTLD_NODELETE,
  // TODO(https://fxbug.dev/323425900): support glibc's RTLD_DEEPBIND flag.
  // kDEEPBIND = RTLD_DEEPBIND,
};

// Masks used to validate flag values.
inline constexpr int kOpenSymbolScopeMask = OpenSymbolScope::kLocal | OpenSymbolScope::kGlobal;
inline constexpr int kOpenBindingModeMask = OpenBindingMode::kLazy | OpenBindingMode::kNow;
inline constexpr int kOpenFlagsMask = OpenFlags::kNoload | OpenFlags::kNodelete;

class RuntimeDynamicLinker {
 public:
  using Soname = elfldltl::Soname<>;

  // Not copyable, not movable
  RuntimeDynamicLinker() = default;
  RuntimeDynamicLinker(const RuntimeDynamicLinker&) = delete;
  RuntimeDynamicLinker(RuntimeDynamicLinker&&) = delete;

  // Attempt to find the loaded module with the given name, returning a nullptr
  // if the module was not found.
  ModuleHandle* FindModule(Soname name);

  template <class OSImpl>
  fit::result<Error, void*> Open(const char* file, int mode) {
    auto already_loaded = CheckOpen(file, mode);
    if (already_loaded.is_error()) [[unlikely]] {
      return already_loaded.take_error();
    }
    // If the module for `file` was found, return a reference to it.
    if (already_loaded.value()) {
      return fit::ok(already_loaded.value());
    }

    // TODO(caslyn): Roll up ModuleHandle allocation with LoadModule allocation,
    // and add comment on the allocation/relationship.
    fbl::AllocChecker ac;
    auto module = ModuleHandle::Create(Soname{file}, ac);
    if (!ac.check()) [[unlikely]] {
      return Error::OutOfMemory();
    }

    // TODO(https://fxbug.dev/324650368): implement file retrieval interfaces.
    // Use a non-scoped diagnostics object for the main module. Because errors
    // are generated on this module directly, its name does not need to be
    // prefixed to the error, as is the case using ld::ScopedModuleDiagnostics.
    dl::Diagnostics diag;
    LoadModule<OSImpl> load_module{std::move(module)};
    if (!load_module.Load(diag)) [[unlikely]] {
      return diag.take_error();
    }

    // TODO(caslyn): these are actions needed to finish loading the module and
    // return its reference back to the caller, but are not meant to be invoked
    // directly by this function. These will be abstracted away eventually.
    auto loaded_module = std::move(load_module).take_module();
    ModuleHandle* module_ref = loaded_module.get();
    loaded_modules_.push_back(std::move(loaded_module));

    return diag.ok(module_ref);
  }

  fit::result<Error, void*> LookupSymbol(ModuleHandle* module, const char* ref);

 private:
  // Perform basic argument checking and check whether a module for `file` was
  // already loaded. An error is returned if bad input was given. Otherwise,
  // return a reference to the module if it was already loaded, or nullptr if
  // a module for `file` was not found.
  fit::result<Error, ModuleHandle*> CheckOpen(const char* file, int mode);

  // The RuntimeDynamicLinker owns the list of all 'live' modules that have been
  // loaded into the system image.
  // TODO(https://fxbug.dev/324136831): support startup modules
  fbl::DoublyLinkedList<std::unique_ptr<ModuleHandle>> loaded_modules_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
