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

  // TODO(https://fxbug.dev/339037138): Add a test exercising the system error
  // case and include it as an example for the fit::error{Error} description.
  // Open `file` with the given `mode`, returning a pointer to the loaded module
  // for the file. The `retrieve_file` argument is passed on to LoadModule.Load
  // and is called as a
  // `fit::result<std::optional<Error>, File>(Diagnostics&, std::string_view)`
  // with the following semantics:
  //   - fit::error{std::nullopt} is a not found error
  //   - fit::error{Error} is an error type that can be passed to
  //     Diagnostics::SystemError (see <lib/elfldltl/diagnostics.h>) to give
  //     more context to the error message.
  //   - fit::ok{File} is the found elfldltl File API type for the module
  //     (see <lib/elfldltl/memory.h>).
  // The Diagnostics reference passed to `retrieve_file` is not used by the
  // function itself to report its errors, but is plumbed into the created File
  // API object that will use it for reporting file read errors.
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
    // file and all its dependencies.

    // Use a non-scoped diagnostics object for the main module. Because errors
    // are generated on this module directly, it's name does not need to be
    // prefixed to the error, as is the case using ld::ScopedModuleDiagnostics.
    dl::Diagnostics diag;
    auto pending_load_modules =
        Load<Loader>(diag, Soname{file}, std::forward<RetrieveFile>(retrieve_file));
    if (pending_load_modules.is_empty()) [[unlikely]] {
      return diag.take_error();
    }

    if (!Relocate(diag, pending_load_modules)) {
      return diag.take_error();
    }

    // TODO(caslyn): These are actions needed to return a reference to the main
    // module back to the caller and add the loaded modules to the dynamic
    // linker's bookkeeping. This will be abstracted away into another function
    // eventually.
    while (!pending_load_modules.is_empty()) {
      auto pending_load_module = pending_load_modules.pop_front();
      auto module = std::move(*pending_load_module).take_module();
      modules_.push_back(std::move(module));
    }

    // TODO(caslyn): This assumes the dlopen-ed module is the first module in
    // modules_.
    return diag.ok(&modules_.front());
  }

 private:
  // Perform basic argument checking and check whether a module for `file` was
  // already loaded. An error is returned if bad input was given. Otherwise,
  // return a reference to the module if it was already loaded, or nullptr if
  // a module for `file` was not found.
  fit::result<Error, ModuleHandle*> CheckOpen(const char* file, int mode);

  // Load the root module and all its dependencies, returning the list of all
  // loaded modules. Starting with the file that was `dlopen`-ed, a LoadModule
  // is created for each file that is to be loaded, decoded, and its
  // dependencies parsed and enqueued to be processed in the same manner.
  // The `retrieve_file` argument is a callable passed down from `Open` and is
  // invoked to retrieve the module's file from the file system for processing.
  template <class Loader, typename RetrieveFile>
  static ModuleList<LoadModule<Loader>> Load(Diagnostics& diag, Soname soname,
                                             RetrieveFile&& retrieve_file) {
    static_assert(std::is_invocable_v<RetrieveFile, Diagnostics&, std::string_view>);

    // This is the list of modules to load and process. The first module of this
    // list will always be the file that was `dlopen`-ed.
    ModuleList<LoadModule<Loader>> load_modules;

    // This lambda will retrieve the module's file, load the module into the
    // system image, and then create a new LoadModule for each of its
    // dependencies and enqueue it onto the `load_modules` list. A
    // fit::result<bool> is returned to the caller where the boolean indicates
    // if the file was found, so that the caller can handle the "not-found"
    // error case.
    auto load_and_enqueue_deps = [&](auto& module) -> fit::result<bool> {
      auto file = retrieve_file(diag, module.name().str());
      if (file.is_error()) [[unlikely]] {
        // Check if the error is a not-found error or a system error.
        if (auto error = file.error_value()) {
          // If a general system error occurred, emit the error for the module.
          diag.SystemError("cannot open ", module.name().str(), ": ", *error);
          return fit::error(false);
        }
        // A "not-found" error occurred, and the caller is responsible for
        // emitting the error message for the module.
        return fit::error(true);
      }

      auto result = module.Load(diag, *std::move(file));
      if (!result) [[unlikely]] {
        return fit::error(false);
      }

      for (const auto& needed_entry : *result) {
        // TODO(https://fxbug.dev/333573264): Check if the module was already
        // loaded by a previous dlopen call or at startup.
        // Skip if this dependency was already added to the load_modules list.
        if (std::find(load_modules.begin(), load_modules.end(), needed_entry) !=
            load_modules.end()) {
          continue;
        }

        fbl::AllocChecker ac;
        auto load_module = LoadModule<Loader>::Create(needed_entry, ac);
        if (!ac.check()) [[unlikely]] {
          diag.OutOfMemory("LoadModule", sizeof(LoadModule<Loader>));
          return fit::error(false);
        }
        load_modules.push_back(std::move(load_module));
      }

      return fit::ok();
    };

    // TODO(https://fxbug.dev/338123289): Separate LoadModule::Create and
    // ModuleHandle::Create so that failed allocations can be handled
    // separately.
    fbl::AllocChecker ac;
    auto root_module = LoadModule<Loader>::Create(soname, ac);
    if (!ac.check()) [[unlikely]] {
      diag.OutOfMemory("LoadModule", sizeof(LoadModule<Loader>));
      return {};
    }

    load_modules.push_back(std::move(root_module));

    // Load the root module and enqueue all its dependencies.
    if (auto result = load_and_enqueue_deps(load_modules.front()); result.is_error()) {
      if (result.error_value()) {
        diag.SystemError(load_modules.front().name().str(), " not found");
      }
      return {};
    }

    // Proceed to load and enqueue the root module's dependencies and their
    // dependencies in a breadth-first order.
    for (auto it = std::next(load_modules.begin()); it != load_modules.end(); it++) {
      if (auto result = load_and_enqueue_deps(*it); result.is_error()) {
        if (result.error_value()) {
          // TODO(https://fxbug.dev/336633049): harmonize this error message
          // with musl, which appends a "(needed by <depending module>)" to the
          // message.
          diag.MissingDependency(it->name().str());
        }
        return {};
      }
    }

    return load_modules;
  }

  // TODO(https://fxbug.dev/324136831): Include global modules in `modules`.
  // Perform relocations on all pending modules to be loaded. Return a boolean
  // if relocations succeeded on all modules.
  template <class Loader>
  bool Relocate(Diagnostics& diag, ModuleList<LoadModule<Loader>>& modules) {
    // Scope diagnostics to the root module so that its name will prefix error
    // messages.
    auto relocate = [&](auto& module) -> bool {
      ld::ScopedModuleDiagnostics root_module_diag{diag, module.name().str()};
      return module.Relocate(diag, modules);
    };
    return std::all_of(std::begin(modules), std::end(modules), relocate);
  }

  // The RuntimeDynamicLinker owns the list of all 'live' modules that have been
  // loaded into the system image.
  // TODO(https://fxbug.dev/324136831): support startup modules
  ModuleList<ModuleHandle> modules_;
};

}  // namespace dl

#endif  // LIB_DL_RUNTIME_DYNAMIC_LINKER_H_
