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

  // Lookup a symbol from the given module, returning a pointer to it in memory,
  // or an error if not found (ie undefined symbol).
  fit::result<Error, void*> LookupSymbol(ModuleHandle* module, const char* ref);

  // Open `file` with the given `mode`, returning a pointer to the loaded module
  // for the file. The `retrieve_file` argument is a callable to be passed on to
  // LoadModule::Load to perform the file retrieval.
  template <class Loader, typename RetrieveFile>
  fit::result<Error, void*> Open(const char* file, int mode, RetrieveFile&& retrieve_file) {
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
    auto load_modules = Load<Loader>(diag, Soname{file}, std::forward<RetrieveFile>(retrieve_file));
    if (load_modules.is_empty()) [[unlikely]] {
      return diag.take_error();
    }

    if (!Relocate(diag, load_modules)) {
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
  template <class Loader, typename RetrieveFile>
  static ModuleList<LoadModule<Loader>> Load(Diagnostics& diag, Soname soname,
                                             RetrieveFile&& retrieve_file) {
    // This is the list of modules to load and process. The first module of this
    // list will always be the main file that was `dlopen`-ed.
    ModuleList<LoadModule<Loader>> pending_modules;

    // TODO(https://fxbug.dev/338123289): Separate LoadModule::Create and
    // ModuleHandle::Create so that failed allocations can be handled
    // separately.
    fbl::AllocChecker ac;
    auto main_module = LoadModule<Loader>::Create(soname, ac);
    if (!ac.check()) [[unlikely]] {
      diag.OutOfMemory("LoadModule", sizeof(LoadModule<Loader>));
      return {};
    }

    pending_modules.push_back(std::move(main_module));

    // TODO(https://fxbug.dev/333573264): This needs to handle if a dep is
    // already loaded.
    // Iterate over the pending modules that need to be loaded and dependencies
    // enqueued, appending each new dependency to the pending_modules list so
    // it can eventually be loaded and processed.
    for (auto it = pending_modules.begin(); it != pending_modules.end(); it++) {
      // TODO(caslyn): support scoped module diagnostics.
      // ld::ScopedModuleDiagnostics module_diag{diag, module_->name().str()};

      auto result = it->Load(diag, retrieve_file);
      if (!result) {
        return {};
      }

      for (const auto& needed_entry : *result) {
        // Skip if this dependency was already added to the pending_modules list.
        if (std::find(pending_modules.begin(), pending_modules.end(), needed_entry) !=
            pending_modules.end()) {
          continue;
        }

        fbl::AllocChecker ac;
        auto load_module = LoadModule<Loader>::Create(needed_entry, ac);
        if (!ac.check()) [[unlikely]] {
          diag.OutOfMemory("LoadModule", sizeof(LoadModule<Loader>));
          return {};
        }
        pending_modules.push_back(std::move(load_module));
      }
    }

    return pending_modules;
  }

  // TODO(https://fxbug.dev/324136831): Include global modules in `modules`.
  // Perform relocations on all pending modules to be loaded. Return a boolean
  // if relocations succeeded on all modules.
  template <class Loader>
  bool Relocate(Diagnostics& diag, ModuleList<LoadModule<Loader>>& modules) {
    auto relocate = [&](auto& module) -> bool { return module.Relocate(diag, modules); };
    return std::all_of(std::begin(modules), std::end(modules), relocate);
  }

  // The RuntimeDynamicLinker owns the list of all 'live' modules that have been
  // loaded into the system image.
  // TODO(https://fxbug.dev/324136831): support startup modules
  ModuleList<ModuleHandle> loaded_modules_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
