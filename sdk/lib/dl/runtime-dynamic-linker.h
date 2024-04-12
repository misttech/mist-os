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

template <class ModuleType>
using ModuleList = fbl::DoublyLinkedList<std::unique_ptr<ModuleType>>;

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

  // Lookup a symbol from the given module, returning a pointer to it in memory,
  // or an error if not found (ie undefined symbol).
  fit::result<Error, void*> LookupSymbol(ModuleHandle* module, const char* ref);

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

    // A ModuleHandle for `file` does not yet exist; proceed to loading the
    // file and all it's dependencies.

    // Use a non-scoped diagnostics object for the main module. Because errors
    // are generated on this module directly, it's name does not need to be
    // prefixed to the error, as is the case using ld::ScopedModuleDiagnostics.
    dl::Diagnostics diag;
    auto load_modules = Load<OSImpl>(diag, Soname{file});
    if (load_modules.is_empty()) [[unlikely]] {
      return diag.take_error();
    }

    // TODO(caslyn): These are actions needed to return a reference to the main
    // module back to the caller and add the loaded modules to the dynamic
    // linker's bookkeeping. This will be abstracted away into another function
    // eventually.
    while (!load_modules.is_empty()) {
      auto load_module = load_modules.pop_front();
      auto module = std::move(*load_module).take_module();
      loaded_modules_.push_back(std::move(module));
    }

    // TODO(caslyn): This assumes the dlopen-ed module is the first module in
    // loaded_modules_.
    return diag.ok(&loaded_modules_.front());
  }

 private:
  // Perform basic argument checking and check whether a module for `file` was
  // already loaded. An error is returned if bad input was given. Otherwise,
  // return a reference to the module if it was already loaded, or nullptr if
  // a module for `file` was not found.
  fit::result<Error, ModuleHandle*> CheckOpen(const char* file, int mode);

  // Load the main module and all its dependencies, returning the list of all
  // loaded modules. Starting with the main file that was `dlopen`-ed, a
  // LoadModule is created for each file that is to be loaded, decoded, and its
  // dependencies parsed and enqueued to be processed in the same manner.
  template <class OSImpl>
  static fbl::DoublyLinkedList<std::unique_ptr<LoadModule<OSImpl>>> Load(Diagnostics& diag,
                                                                         Soname soname) {
    // This is the list of modules to load and process. The first module of this
    // list will always be the main file that was `dlopen`-ed.
    ModuleList<LoadModule<OSImpl>> pending_modules;

    fbl::AllocChecker ac;
    auto load_module = LoadModule<OSImpl>::Create(soname, ac);
    if (!ac.check()) [[unlikely]] {
      // TODO(caslyn): communicate out of memory error.
      return {};
    }

    pending_modules.push_back(std::move(load_module));

    for (auto it = pending_modules.begin(); it != pending_modules.end(); it++) {
      // TODO(caslyn): support scoped module diagnostics.
      // ld::ScopedModuleDiagnostics module_diag{diag, module_->name().str()};

      // TODO(caslyn): Eventually Load and Decode will become separate calls.
      if (!it->Load(diag)) {
        return {};
      }
      // TODO(caslyn): Eventually, EnqueueDeps will be a static function on the
      // RuntimeDynamicLinker and will pass in the DecodeResult for the loadmodule.
      if (!it->EnqueueDeps(diag, pending_modules)) {
        return {};
      }
    }

    return pending_modules;
  }

  // The RuntimeDynamicLinker owns the list of all 'live' modules that have been
  // loaded into the system image.
  // TODO(https://fxbug.dev/324136831): support startup modules
  ModuleList<ModuleHandle> loaded_modules_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
