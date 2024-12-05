// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_

#include <lib/fit/result.h>

#include <type_traits>
#include <utility>

#include "diagnostics.h"
#include "link.h"
#include "symbol.h"

namespace elfldltl {

// This type implements a Definition which can be used as the return type for
// the `resolve` parameter for RelocateSymbolic. See link.h for more details.
// The Module type must have the following methods:
//
//  * const SymbolInfo& symbol_info() const
//    Returns the SymbolInfo type associated with this module.
//    This is used to reify the Sym in the referring module to a name.
//
//  * size_type load_bias() const
//    Returns the load bias for symbol addresses in this module.
//
//  * size_type tls_module_id() const
//    Returns the TLS module ID number for this module.
//    This will be zero for a module with no PT_TLS segment.
//    It's always one in the main executable if has a PT_TLS segment,
//    but may be one in a different module if the main executable has none.
//
//  * bool uses_static_tls() const
//    This module may have TLS relocations for IE or LE model accesses.
//
//  * size_type static_tls_bias() const
//    Returns the static TLS layout bias for the defining module.
//
//  * fit::result<bool, TlsDescGot> tls_desc(Diagnostics&, const Sym&, Addend addend) const
//  * fit::result<bool, TlsDescGot> tls_desc(Diagnostics&) const
//    See elfldltl::RelocateSymbolic API comments about the two overloads.
//    This implements that method but for some particular defined symbol in
//    `.symbols_info().symtab()`.
//
//  * fit::result<bool, const Sym*> Lookup(Diagnostics&, SymbolName& name) const
//    This can usually be just `return fit::ok(name.Lookup(symbol_info()));`.
//    It's responsible for looking up a symbol name in the module.
//    It can return failure to declare that resolution has failed entirely.
//    It returns success with nullptr just to indicate this module does not
//    define this symbol name.  The returned pointer must be from symbol_info()
//    so that its string table can be used.
//
template <class Module, typename TlsDescResolver>
struct ResolverDefinition {
  using Elf = std::decay_t<decltype(std::declval<Module>().symbol_info())>::Elf;
  using Addr = Elf::Addr;
  using Addend = Elf::Addend;
  using Sym = Elf::Sym;

  static constexpr ResolverDefinition UndefinedWeak(TlsDescResolver* tlsdesc_resolver = nullptr) {
    static_assert(ResolverDefinition{}.undefined_weak());
    return {.tlsdesc_resolver_ = tlsdesc_resolver};
  }

  // This should be called before any other method to check if this Definition is valid.
  constexpr bool undefined_weak() const { return !symbol_; }

  constexpr const Sym& symbol() const { return *symbol_; }
  constexpr auto bias() const { return module_->load_bias(); }

  constexpr auto tls_module_id() const { return module_->tls_module_id(); }
  constexpr bool uses_static_tls() const { return module_->uses_static_tls(); }
  constexpr auto static_tls_bias() const { return module_->static_tls_bias(); }

  template <
      class Diagnostics, typename T = TlsDescResolver,
      typename = std::enable_if_t<std::is_invocable_v<T, Diagnostics&, const ResolverDefinition&>>>
  constexpr auto tls_desc(Diagnostics& diag) const {
    return (*tlsdesc_resolver_)(diag, *this);
  }

  template <class Diagnostics, typename T = TlsDescResolver,
            typename = std::enable_if_t<
                std::is_invocable_v<T, Diagnostics&, const ResolverDefinition&, Addend>>>
  constexpr auto tls_desc(Diagnostics& diag, Addend addend) const {
    return (*tlsdesc_resolver_)(diag, *this, addend);
  }

  template <typename T = TlsDescResolver, typename = std::enable_if_t<std::is_invocable_v<T>>>
  constexpr auto tls_desc_undefined_weak() const {
    return (*tlsdesc_resolver_)();
  }

  template <typename T = TlsDescResolver,
            typename = std::enable_if_t<std::is_invocable_v<T, Addend>>>
  constexpr auto tls_desc_undefined_weak(Addend addend) const {
    return (*tlsdesc_resolver_)(addend);
  }

  const Sym* symbol_ = nullptr;
  const Module* module_ = nullptr;
  TlsDescResolver* tlsdesc_resolver_ = nullptr;
};

enum class ResolverPolicy : bool {
  // The first symbol found takes precedence, searching ends after finding the
  // first.
  kStrictLinkOrder,

  // This follows LD_DYNAMIC_WEAK=1 semantics, the resolver will resolve to the
  // first STB_GLOBAL symbol even if an STB_WEAK symbol was seen earlier.
  // If no global symbol was found the first STB_WEAK symbol will prevail.
  kStrongOverWeak,
};

// Returns a callable object which can be used for RelocateSymbolic's `resolve`
// argument.  This takes some Module object (as described above) whose
// symbol_info() contains the symbol given by RelocateSymbolic.  The `modules`
// argument is a list of modules from where symbolic definitions can be
// resolved, this list is in order of precedence.  The ModuleList type is a
// forward iterable range or container.  diag is a diagnostics object for
// reporting errors.  The TlsDescResolver is a callable object that's called as
// `fit::result<bool, TlsDescGot>(Diagnostics&, const Definition&, Addend)` or
// `fit::result<bool, TlsDescGot>(Diagnostics&, const Definition&)` for a
// TLSDESDC relocation resolved to a defined symbol; and as `TlsDescGot()` or
// `TlsDescGot(Addend)` for one resolved as an undefined weak reference.
//
// All references passed to elfldltl::MakeSymbolResolver should outlive the
// returned object, which in turn must outlive its return values (Definition
// objects).  The tlsdesc_resolver reference is saved in Definition objects so
// it can be called from the RelocateSymbolic callbacks.
template <class Module, class ModuleList, class Diagnostics, typename TlsDescResolver>
constexpr auto MakeSymbolResolver(const Module& ref_module, ModuleList& modules, Diagnostics& diag,
                                  TlsDescResolver& tlsdesc_resolver,
                                  ResolverPolicy policy = ResolverPolicy::kStrictLinkOrder) {
  using Definition = ResolverDefinition<Module, TlsDescResolver>;

  return [&ref_module, &modules, &diag, &tlsdesc_resolver, policy](
             const auto& ref, RelocateTls tls_type) -> fit::result<bool, Definition> {
    if (ref.runtime_local()) {
      // The symbol just resolves to itself in the referring module.  Usually
      // this would have been replaced with an R_*_RELATIVE reloc (and then
      // folded into DT_RELR), but it doesn't have to be.  In practice, this
      // comes up for TLS relocations which still need to have their specific
      // reloc type but can be for purely module-local references.
      return fit::ok(Definition{&ref, &ref_module});
    }

    SymbolName name{ref_module.symbol_info(), ref};

    // Return the chosen Definition after some checking.
    auto use = [&ref_module, tls_type, &diag, &tlsdesc_resolver,
                &name](Definition def) -> fit::result<bool, Definition> {
      switch (tls_type) {
        case RelocateTls::kNone:
          if (def.symbol_->type() == ElfSymType::kTls) [[unlikely]] {
            return fit::error{
                diag.FormatError("non-TLS relocation resolves to STT_TLS symbol ", name)};
          }
          break;
        case RelocateTls::kStatic:
          // If the referring module itself must be in the initial exec set
          // then it's fine for it to use IE relocs for any of its references.
          // If the referring module itself does not have DF_STATIC_TLS set to
          // prevent it from being loaded outside the initial exec set, then
          // the defining module must be guaranteed to be in the initial exec
          // set.  Note that we expect a main executable module to always
          // return true for uses_static_tls() even though the linker doesn't
          // set DF_STATIC_TLS when generating relocs in an executable
          // (including PIE), so we're really using uses_static_tls() here as a
          // proxy for "is in initial exec set".
          if (!ref_module.uses_static_tls() && !def.module_->uses_static_tls()) [[unlikely]] {
            return fit::error{diag.FormatError(
                "TLS Initial Exec relocation resolves to STT_TLS symbol in module without DF_STATIC_TLS: ",
                name)};
          }
          [[fallthrough]];
        case RelocateTls::kDynamic:
        case RelocateTls::kDesc:
          if (def.symbol_->type() != ElfSymType::kTls) [[unlikely]] {
            return fit::error{
                diag.FormatError("TLS relocation resolves to non-STT_TLS symbol: ", name)};
          }
          break;
      }
      if (tls_type == RelocateTls::kDesc) {
        def.tlsdesc_resolver_ = &tlsdesc_resolver;
      }
      return fit::ok(def);
    };

    if (name.empty()) [[unlikely]] {
      return fit::error{diag.FormatError("Symbol had invalid st_name")};
    }

    Definition weak_def = Definition::UndefinedWeak(&tlsdesc_resolver);
    for (const auto& module : modules) {
      auto lookup = module.Lookup(diag, name);
      if (lookup.is_error()) [[unlikely]] {
        // The module's hook said to fail the whole resolution, which to the
        // caller likely means the entire relocation of the referring module.
        // The error value will tell the caller whether to bail out now or
        // continue at a higher level, such as relocating other modules.
        return lookup.take_error();
      }

      if (const auto* sym = lookup.value()) {
        const Definition module_def{sym, &module};
        switch (sym->bind()) {
          case ElfSymBind::kWeak:
            // In kStrongOverWeak policy the first weak definition will prevail
            // if no strong definition is found later.
            if (policy == ResolverPolicy::kStrongOverWeak) {
              if (weak_def.undefined_weak()) {
                weak_def = module_def;
              }
              continue;
            }
            [[fallthrough]];
          case ElfSymBind::kGlobal:
            // The first (strong) global always prevails regardless of policy.
            return use(module_def);
          case ElfSymBind::kLocal:
            // Local symbols are never matched by name.
            if (!diag.FormatWarning("STB_LOCAL found in hash table")) {
              return fit::error{false};
            }
            continue;
          case ElfSymBind::kUnique:
            if (!diag.FormatError("STB_GNU_UNIQUE not supported")) {
              return fit::error{false};
            }
            break;
          default:
            if (!diag.FormatError("Unknown symbol binding type",
                                  static_cast<unsigned>(sym->bind()))) {
              return fit::error{false};
            }
            break;
        }

        // That returned a definition or continued to look for another so this
        // is only reached for the error cases where the Diagnostics object
        // said to keep going.
        [[unlikely]] return fit::error{true};
      }
    }

    if (!weak_def.undefined_weak()) {
      // The only definition found was weak in kStrongOverWeak mode.
      return use(weak_def);
    }

    // Undefined weak is a valid return value for an STB_WEAK reference.
    if (ref.bind() == ElfSymBind::kWeak) [[likely]] {
      return fit::ok(weak_def);
    }

    return fit::error{diag.UndefinedSymbol(name)};
  };
}

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_
