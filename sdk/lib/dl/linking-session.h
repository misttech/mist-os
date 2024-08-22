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

// SessionModule is the temporary data structure created to load a file; a
// SessionModule is created when a file needs to be loaded, and is destroyed after
// the file module and all its dependencies have been loaded, decoded, symbols
// resolved, and relro protected.
template <class Loader>
class SessionModule;

// A list of unique "temporary" SessionModule data structures used for loading a
// file.
template <class Loader>
using SessionModuleList = fbl::DoublyLinkedList<std::unique_ptr<SessionModule<Loader>>>;

// A LinkingSession encapsulates the decoding, loading, relocation and creation
// of RuntimeModules from a single dlopen call. A LinkingSession instance only
// lives as long as the dlopen call, and a successful LinkingSession will
// provide the list of RuntimeModules to its caller (see LinkingSession::Commit).
template <class Loader>
class LinkingSession {
 public:
  // Not copyable, not movable.
  LinkingSession(const LinkingSession&) = delete;
  LinkingSession(LinkingSession&&) = delete;
  LinkingSession() = default;

  // The caller calls Commit() to finalize the LinkingSession after it has
  // loaded and linked all the modules needed for a single dlopen call. This
  // will transfer ownership of the RuntimeModules created during this session
  // to the caller.
  ModuleList Commit() && { return std::move(runtime_modules_); }

  // TODO(https://fxbug.dev/333573264): Talk about how previously-loaded modules
  // are handled in this function.
  // Load the root module and all its dependencies. The `retrieve_file` argument
  // is a callable passed down from `Open` and is invoked to retrieve the
  // module's file from the file system for processing.
  template <typename RetrieveFile>
  bool Load(Diagnostics& diag, Soname soname, RetrieveFile&& retrieve_file,
            const ModuleList& loaded_modules) {
    static_assert(std::is_invocable_v<RetrieveFile, Diagnostics&, std::string_view>);

    // The root module will always be the first module in the list.
    if (!EnqueueModule(diag, soname, loaded_modules)) {
      return false;
    }

    // This lambda will retrieve the module's file, load the module into the
    // system image, and then create new modules for each of its dependencies
    // to enqueue onto session_modules_ for future processing. A
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

      if (auto result = module.Load(diag, *std::move(file))) {
        // Create a module for each dependency from the SessionModule.Load result
        // and enqueue it onto session_modules_ to be processed and loaded in the
        // future.
        auto enqueue_dep = [this, &diag, &loaded_modules](const Soname& name) {
          return EnqueueModule(diag, name, loaded_modules);
        };
        if (std::all_of(std::begin(*result), std::end(*result), enqueue_dep)) {
          return fit::ok();
        }
      }

      return fit::error(false);
    };

    // Load the root module and enqueue all its dependencies.
    if (auto result = load_and_enqueue_deps(session_modules_.front()); result.is_error()) {
      if (result.error_value()) {
        diag.SystemError(soname.str(), " not found");
      }
      return false;
    }

    // Proceed to load and enqueue the root module's dependencies and their
    // dependencies in a breadth-first order.
    for (auto it = std::next(session_modules_.begin()); it != session_modules_.end(); it++) {
      if (auto result = load_and_enqueue_deps(*it); result.is_error()) {
        if (result.error_value()) {
          // TODO(https://fxbug.dev/336633049): harmonize this error message
          // with musl, which appends a "(needed by <depending module>)" to the
          // message.
          diag.MissingDependency(it->name().str());
        }
        return false;
      }
    }

    return true;
  }

  bool EnqueueModule(Diagnostics& diag, Soname soname, const ModuleList& loaded_modules) {
    if (std::find(session_modules_.begin(), session_modules_.end(), soname) !=
        session_modules_.end()) {
      // The module was already added to session_modules_ in this LinkingSession.
      return true;
    }

    // TODO(https://fxbug.dev/333573264): Check if the module was already
    // loaded by a previous dlopen call or at startup and use that reference
    // instead.

    // TODO(https://fxbug.dev/338229987): This is just to make sure we're not
    // exercising deps from modules already loaded yet.
    assert(std::find(loaded_modules.begin(), loaded_modules.end(), soname) == loaded_modules.end());

    fbl::AllocChecker module_ac;
    auto module = RuntimeModule::Create(module_ac, soname);
    if (!module_ac.check()) [[unlikely]] {
      diag.OutOfMemory("permanent module data structure", sizeof(RuntimeModule));
      return false;
    }
    fbl::AllocChecker session_module_ac;
    auto session_module = SessionModule<Loader>::Create(session_module_ac, *module);
    if (!session_module_ac.check()) [[unlikely]] {
      diag.OutOfMemory("temporary module data structure", sizeof(SessionModule<Loader>));
      return false;
    }

    runtime_modules_.push_back(std::move(module));
    session_modules_.push_back(std::move(session_module));

    return true;
  }

  // TODO(https://fxbug.dev/324136831): Include global modules.
  // Perform relocations on all pending modules to be loaded. Return a boolean
  // if relocations succeeded on all modules.
  bool Relocate(Diagnostics& diag) {
    auto relocate_and_relro = [&](auto& session_module) -> bool {
      // TODO(https://fxbug.dev/339662473): this doesn't use the root module's
      // name in the scoped diagnostics. Add test for missing transitive symbol
      // and make sure the correct name is used in the error message.
      ld::ScopedModuleDiagnostics root_module_diag{diag, session_module.name().str()};
      return session_module.Relocate(diag, session_modules_) && session_module.ProtectRelro(diag);
    };
    return std::all_of(std::begin(session_modules_), std::end(session_modules_),
                       relocate_and_relro);
  }

 private:
  // The list of "temporary" SessionModules needed to perform loading, decoding,
  // relocations, etc during this LinkingSession. There is a 1:1 mapping between
  // elements in session_modules_ and runtime_modules_: each element in
  // this list is responsible for filling out the runtime and ABI data for the
  // corresponding RuntimeModule located at the same index in runtime_modules_.
  // In other words, session_modules_[idx].runtime_module() is a reference to
  // the runtime module at runtime_modules_[idx]. Unlike runtime_modules_, this
  // list will live only as long as this LinkingSession instance.
  SessionModuleList<Loader> session_modules_;

  // The list of "permanent" RuntimeModules created during this LinkingSession.
  // Its ownership is transferred to the RuntimeDynamicLinker when
  // LinkingSession::Commit is called, otherwise this list will get gc'ed with
  // the LinkingSession.
  ModuleList runtime_modules_;
};

template <class Loader>
class SessionModule : public ld::LoadModule<ld::DecodedModuleInMemory<>>,
                      public fbl::DoublyLinkedListable<std::unique_ptr<SessionModule<Loader>>> {
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

  // The SessionModule::Create(...) takes a reference to the Module for the
  // file, setting information on it during the loading, decoding, and
  // relocation process.
  [[nodiscard]] static std::unique_ptr<SessionModule> Create(fbl::AllocChecker& ac,
                                                             RuntimeModule& runtime_module) {
    std::unique_ptr<SessionModule> session_module{new (ac) SessionModule(runtime_module)};
    if (session_module) [[likely]] {
      session_module->set_name(runtime_module.name());
      // Have the underlying DecodedModule (see <lib/ld/decoded-module.h>) point to
      // the ABIModule embedded in the Module, so that its information will
      // be filled out during decoding operations.
      session_module->decoded().set_module(runtime_module.module());
    }
    return session_module;
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
  bool Relocate(Diagnostics& diag, SessionModuleList<Loader>& session_modules) {
    constexpr NoTlsDesc kNoTlsDesc{};
    auto memory = ld::ModuleMemory{module()};
    auto resolver = elfldltl::MakeSymbolResolver(*this, session_modules, diag, kNoTlsDesc);
    return elfldltl::RelocateRelative(diag, memory, reloc_info(), load_bias()) &&
           elfldltl::RelocateSymbolic(memory, diag, reloc_info(), symbol_info(), load_bias(),
                                      resolver);
  }

  // Apply relro protections. `relro_` cannot be used after this call.
  bool ProtectRelro(Diagnostics& diag) { return std::move(relro_).Commit(diag); }

 private:
  // A SessionModule can only be created with SessionModule::Create...).
  explicit SessionModule(RuntimeModule& runtime_module) : runtime_module_(runtime_module) {}

  // This is a reference to the "permanent" module data structure that this
  // SessionModule is responsible for: runtime information is set on the
  // `runtime_module_` during the course of the loading process. Whereas this
  // SessionModule instance will get destroyed at the end of `dlopen`,
  // its `runtime_module_` will live as long as the file is loaded, in the
  // RuntimeDynamicLinker's `modules_` list.
  RuntimeModule& runtime_module_;
  // The relro capability that is provided when the module is decoded and is
  // used to apply relro protections after the module is relocated.
  Relro relro_;
};

}  // namespace dl

#endif  // LIB_DL_LINKING_SESSION_H_
