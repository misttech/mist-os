// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_LINKING_SESSION_H_
#define LIB_DL_LINKING_SESSION_H_

#include <lib/elfldltl/resolve.h>
#include <lib/fit/result.h>
#include <lib/ld/decoded-module-in-memory.h>
#include <lib/ld/load-module.h>
#include <lib/ld/tlsdesc.h>

#include <ranges>

#include "concat-view.h"
#include "diagnostics.h"
#include "runtime-module.h"

namespace dl {

// A LinkingSession encapsulates the decoding, loading, relocation and creation
// of RuntimeModules from a single dlopen call. A LinkingSession instance only
// lives as long as the dlopen call, and a successful LinkingSession will
// provide the list of RuntimeModules to its caller (see LinkingSession::Commit).
template <class Loader>
class LinkingSession {
 public:
  using size_type = Elf::size_type;

  // Not copyable, not movable.
  LinkingSession(const LinkingSession&) = delete;
  LinkingSession(LinkingSession&&) = delete;

  // A LinkingSession is provided a reference to the dynamic linker's list of
  // already loaded modules to refer to during the linking procedure, and the
  // max static TLS module id from the passive ABI to use for TLS resolution.
  explicit LinkingSession(const ModuleList& loaded_modules, size_type max_static_tls_modid)
      : loaded_modules_(loaded_modules), tls_desc_resolver_(max_static_tls_modid) {}

  ~LinkingSession() = default;

  template <typename RetrieveFile>
  bool Link(Diagnostics& diag, Soname soname, RetrieveFile&& retrieve_file) {
    if (!Load(diag, soname, std::forward<RetrieveFile>(retrieve_file))) {
      return false;
    }
    // The root module for the dlopen-ed file is always the first module
    // enqueued in this list.
    RuntimeModule& root_module = runtime_modules_.front();
    return root_module.ReifyModuleTree(diag) && Relocate(diag, root_module.module_tree());
  }

  // The caller calls Commit() to finalize the LinkingSession after it has
  // loaded and linked all the modules needed for a single dlopen call. This
  // will transfer ownership of the RuntimeModules created during this session
  // to the caller.
  ModuleList Commit() && { return std::move(runtime_modules_); }

 private:
  // Forward declaration; see definition below.
  class SessionModule;

  using SessionModuleList = fbl::DoublyLinkedList<std::unique_ptr<SessionModule>>;

  class TlsDescResolver : public ld::LocalRuntimeTlsDescResolver {
   public:
    explicit TlsDescResolver(size_type max_static_tls_modid)
        : max_static_tls_modid_(max_static_tls_modid) {}

    TlsDescGot operator()(Addend addend) const {
      return ld::LocalRuntimeTlsDescResolver::operator()(addend);
    }

    fit::result<bool, TlsDescGot> operator()(auto& diag, const auto& defn) const {
      assert(defn.tls_module_id() != 0);
      if (defn.tls_module_id() <= max_static_tls_modid_) {
        return ld::LocalRuntimeTlsDescResolver::operator()(diag, defn);
      }
      return fit::error{
          diag.FormatError("TODO(https://fxbug.dev/342480690): dynamic TLS is not supported")};
    }

   private:
    size_type max_static_tls_modid_;
  };

  // TODO(https://fxbug.dev/333573264): Talk about how previously-loaded modules
  // are handled in this function.
  // Load the root module and all its dependencies. The `retrieve_file` argument
  // is a callable passed down from `Open` and is invoked to retrieve the
  // module's file from the file system for processing.
  template <typename RetrieveFile>
  bool Load(Diagnostics& diag, Soname soname, RetrieveFile&& retrieve_file) {
    static_assert(std::is_invocable_v<RetrieveFile, Diagnostics&, std::string_view>);

    // The root module will always be the first module in the list.
    if (!EnqueueModule(diag, soname)) {
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

      if (auto dep_names = module.Load(diag, *std::move(file))) {
        // Create and enqueue a module for each dependency, skipping
        // dependencies that have already been enqueued. The module that was
        // loaded will also store a reference to the dependency's RuntimeModule
        // in its direct_deps list.
        auto enqueue_dep = [this, &diag, &parent_list = module.runtime_module().direct_deps()](
                               const Soname& name) {
          if (std::find(session_modules_.begin(), session_modules_.end(), name) !=
              session_modules_.end()) {
            return true;
          }
          if (const RuntimeModule* dep = EnqueueModule(diag, name)) {
            return parent_list.push_back(diag, "direct dependency container", dep);
          }
          return false;
        };
        if (std::ranges::all_of(*dep_names, enqueue_dep)) {
          return fit::ok();
        }
      }

      return fit::error(false);
    };

    // Proceed to load and enqueue the root module's dependencies and their
    // dependencies in a breadth-first order.
    for (auto it = session_modules_.begin(); it != session_modules_.end(); ++it) {
      if (auto result = load_and_enqueue_deps(*it); result.is_error()) {
        // If fit::error{true} is returned, this is a not-found error.
        if (result.error_value()) {
          if (it == session_modules_.begin()) {
            diag.SystemError(it->name().str(), " not found");
          } else {
            // TODO(https://fxbug.dev/336633049): harmonize this error message
            // with musl, which appends a "(needed by <depending module>)" to the
            // message.
            diag.MissingDependency(it->name().str());
          }
        }
        return false;
      }
    }

    return true;
  }

  // Create module data structures for `soname` and enqueue the modules onto
  // this LinkingSession's bookkeeping lists.
  const RuntimeModule* EnqueueModule(Diagnostics& diag, Soname soname) {
    if (auto it = std::ranges::find(loaded_modules_, soname, &RuntimeModule::name);
        it != loaded_modules_.end()) {
      // Return a reference to the module if it was already loaded at startup or
      // by a LinkingSession from a previous dlopen() call.
      return &*it;
    }

    fbl::AllocChecker module_ac;
    auto module = RuntimeModule::Create(module_ac, soname);
    if (!module_ac.check()) [[unlikely]] {
      diag.OutOfMemory("permanent module data structure", sizeof(RuntimeModule));
      return nullptr;
    }
    fbl::AllocChecker session_module_ac;
    auto session_module = SessionModule::Create(session_module_ac, *module);
    if (!session_module_ac.check()) [[unlikely]] {
      diag.OutOfMemory("temporary module data structure", sizeof(SessionModule));
      return nullptr;
    }

    runtime_modules_.push_back(std::move(module));
    session_modules_.push_back(std::move(session_module));

    // Return a pointer to the RuntimeModule that was just created and enqueued.
    return &runtime_modules_.back();
  }

  // Perform relocations on all pending modules to be loaded. Return a boolean
  // if relocations succeeded on all modules.
  bool Relocate(Diagnostics& diag, const auto& session_modules) {
    if (session_modules.empty()) {
      return false;
    }

    // Construct a view of modules that will be used for symbol resolution.
    // This is an ordered list of global modules that have already been loaded,
    // followed by the non-global modules being loaded by this session.
    auto loaded_global = std::views::filter(loaded_modules_, &RuntimeModule::is_global);
    auto session_local = std::views::filter(session_modules, &RuntimeModule::is_local);
    auto relocate_and_relro =
        // The concat_view created here will be used as const since the lambda
        // is not mutable--anyway RuntimeModule::Relocate et al take the module
        // list object as const& rather than assuming it's a view.  filter_view
        // doesn't have const overloads so it can't be used as const and thus
        // can't be directly in a const concat_view.  However, ref_view has
        // const overloads that don't need the referenced view to have them.
        [resolution_modules =
             ConcatView{
                 std::ranges::ref_view(loaded_global),
                 std::ranges::ref_view(session_local),
             },
         &diag, this](SessionModule& session_module) -> bool {
      // TODO(https://fxbug.dev/339662473): this doesn't use the root module's
      // name in the scoped diagnostics. Add test for missing transitive symbol
      // and make sure the correct name is used in the error message.
      ld::ScopedModuleDiagnostics root_module_diag{diag, session_module.name().str()};
      return session_module.Relocate(diag, resolution_modules, tls_desc_resolver_) &&
             session_module.ProtectRelro(diag);
    };
    return std::all_of(std::begin(session_modules_), std::end(session_modules_),
                       relocate_and_relro);
  }

  // The list of "temporary" SessionModules needed to perform loading,
  // decoding, relocations, etc during this LinkingSession.  There is a 1:1
  // mapping between elements in session_modules_ and runtime_modules_: each
  // element in this list is responsible for filling out the runtime and ABI
  // data for the corresponding RuntimeModule located at the same index in
  // runtime_modules_.  In other words, session_modules_[idx].runtime_module()
  // is a reference to the runtime module at runtime_modules_[idx]. Unlike
  // runtime_modules_, this list will live only as long as this LinkingSession
  // instance.
  SessionModuleList session_modules_;

  // The list of "permanent" RuntimeModules created during this LinkingSession.
  // Its ownership is transferred to the RuntimeDynamicLinker when
  // LinkingSession::Commit is called, otherwise this list will get gc'ed with
  // the LinkingSession.
  ModuleList runtime_modules_;

  // The set of loaded modules at the time this LinkingSession instance was
  // created.
  const ModuleList& loaded_modules_;

  // TODO(https://fxbug.dev/342480690): Support Dynamic TLS.
  // The resolver used for TLSDESC relocations.
  const TlsDescResolver tls_desc_resolver_;
};

// SessionModule is the temporary data structure created to load a file and
// perform relocations for a new module. A SessionModule is managed by
// session_modules_ and will get destroyed with the LinkingSession instance.
template <class Loader>
class LinkingSession<Loader>::SessionModule
    : public ld::LoadModule<ld::DecodedModuleInMemory<>>,
      public fbl::DoublyLinkedListable<std::unique_ptr<SessionModule>> {
 public:
  using Relro = typename Loader::Relro;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<ld::kMaxSegments>::Container>;
  using TlsDescGot = Elf::TlsDescGot<>;

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
  // the the ABI module. A vector of Soname objects of the module's DT_NEEDEDs
  // are returned to the caller.
  template <class File>
  std::optional<Vector<Soname>> Load(Diagnostics& diag, File&& file) {
    // Read the file header and program headers into stack buffers and map in
    // the image.  This fills in load_info() as well as the module vaddr bounds
    // and phdrs fields.
    Loader loader;
    auto headers = decoded().LoadFromFile(diag, loader, std::forward<File>(file));
    if (!headers) [[unlikely]] {
      return {};
    }

    Vector<size_type> needed_offsets;
    // TODO(https://fxbug.dev/331421403): TLS is not supported yet.
    size_type max_tls_modid = 0;
    if (!decoded().DecodeFromMemory(  //
            diag, loader.memory(), loader.page_size(), *headers, max_tls_modid,
            elfldltl::DynamicRelocationInfoObserver(decoded().reloc_info()),
            NeededObserver(needed_offsets))) [[unlikely]] {
      return {};
    }

    // After successfully loading the file, finalize the module's mapping by
    // calling `Commit` on the loader. Save the returned relro capability that
    // will be used to apply relro protections later.
    relro_ = decoded().CommitLoader(std::move(loader));

    // TODO(https://fxbug.dev/366279579): The code that parses the names from
    // the symbol table be shared with <lib/ld/remote-decoded-module.h>.
    Vector<Soname> dep_names;
    if (dep_names.reserve(diag, kNeededError, needed_offsets.size())) [[likely]] {
      for (size_type offset : needed_offsets) {
        std::string_view name = this->symbol_info().string(offset);
        if (name.empty()) [[unlikely]] {
          diag.FormatError("DT_NEEDED has DT_STRTAB offset ", offset, " with DT_STRSZ ",
                           this->symbol_info().strtab().size());
          return {};
        }
        if (!dep_names.push_back(diag, kNeededError, Soname{name})) [[unlikely]] {
          return {};
        }
      }
      return std::move(dep_names);
    }

    return {};
  }

  // Perform relative and symbolic relocations, resolving symbols from the
  // ordered list of modules as needed.
  bool Relocate(Diagnostics& diag, const auto& ordered_modules,
                const TlsDescResolver& tls_desc_resolver) {
    auto memory = ld::ModuleMemory{module()};
    auto resolver =
        elfldltl::MakeSymbolResolver(runtime_module_, ordered_modules, diag, tls_desc_resolver);
    return elfldltl::RelocateRelative(diag, memory, reloc_info(), load_bias()) &&
           elfldltl::RelocateSymbolic(memory, diag, reloc_info(), symbol_info(), load_bias(),
                                      resolver);
  }

  // Apply relro protections. `relro_` cannot be used after this call.
  bool ProtectRelro(Diagnostics& diag) { return std::move(relro_).Commit(diag); }

  constexpr RuntimeModule& runtime_module() const { return runtime_module_; }

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
