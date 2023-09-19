// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_

#include <memory>
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
//  * const SymbolInfo& symbol_info()
//    Returns the SymbolInfo type associated with this module. This is used
//    to call SymbolInfo::Lookup().
//
//  * size_type load_bias()
//    Returns the load bias for symbol addresses in this module.
//
//  * size_type tls_module_id()
//    Returns the TLS module ID number for this module.
//
//  * size_type static_tls_bias()
//    Returns the static TLS layout bias for the defining module.
//
//  * size_type tls_desc_hook(const Sym&), tls_desc_value(const Sym&)
//    Returns the two values for the TLSDESC resolution.
//
template <class Module>
struct ResolverDefinition {
  using Sym = typename std::decay_t<decltype(std::declval<Module>().symbol_info())>::Sym;

  // TODO(fxbug.dev/120388): preferably, this would just be a constexpr static variable
  // but clang can't compile that.
  static constexpr ResolverDefinition UndefinedWeak() {
    static_assert(ResolverDefinition{}.undefined_weak());
    return {};
  }

  // This should be called before any other method to check if this Definition is valid.
  constexpr bool undefined_weak() const { return !symbol_; }

  constexpr const Sym& symbol() const { return *symbol_; }
  constexpr auto bias() const { return module_->load_bias(); }

  constexpr auto tls_module_id() const { return module_->tls_module_id(); }
  constexpr auto static_tls_bias() const { return module_->static_tls_bias(); }
  constexpr auto tls_desc_hook() const { return module_->tls_desc_hook(*symbol_); }
  constexpr auto tls_desc_value() const { return module_->tls_desc_value(*symbol_); }

  const Sym* symbol_ = nullptr;
  const Module* module_ = nullptr;
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
// argument. This takes a SymbolInfo object which is used for finding the name
// of the symbol given by RelocateSymbolic. The `modules` argument is a list of
// modules from where symbolic definitions can be resolved, this list is in
// order of precedence. The ModuleList type is a forward iterable range or
// container. diag is a diagnostics object for reporting errors. All references
// passed to MakeSymbolResolver should outlive the returned object.
template <class SymbolInfo, class ModuleList, class Diagnostics>
constexpr auto MakeSymbolResolver(const SymbolInfo& ref_info, const ModuleList& modules,
                                  Diagnostics& diag,
                                  ResolverPolicy policy = ResolverPolicy::kStrictLinkOrder) {
  using Module = std::decay_t<decltype(*std::declval<ModuleList>().begin())>;
  using Definition = ResolverDefinition<Module>;

  return [&, policy](const auto& ref, elfldltl::RelocateTls tls_type) -> std::optional<Definition> {
    // TODO(fxbug.dev/118060): Support thread local symbols. For now we just use
    // FormatError, which isn't preferable, but this is just a temporary error.
    if (tls_type != RelocateTls::kNone) {
      diag.FormatError("TLS not yet supported");
      return std::nullopt;
    }

    elfldltl::SymbolName name{ref_info, ref};

    if (name.empty()) [[unlikely]] {
      diag.FormatError("Symbol had invalid st_name");
      return std::nullopt;
    }

    Definition weak_def = Definition::UndefinedWeak();
    for (const auto& module : modules) {
      if (const auto* sym = name.Lookup(module.symbol_info())) {
        const Definition module_def{sym, &module};
        switch (sym->bind()) {
          case ElfSymBind::kWeak:
            // In kStrongOverWeak policy the first weak definition will prevail
            // if no global symbol is found later.
            if (policy == ResolverPolicy::kStrongOverWeak) {
              if (weak_def.undefined_weak()) {
                weak_def = module_def;
              }
              break;
            }
            [[fallthrough]];
          case ElfSymBind::kGlobal:
            // The first (strong) global always prevails regardless of policy.
            return module_def;
          case ElfSymBind::kLocal:
            diag.FormatWarning("STB_LOCAL found in hash table");
            break;
          case ElfSymBind::kUnique:
            diag.FormatError("STB_GNU_UNIQUE not supported");
            return {};
          default:
            diag.FormatError("Unkown symbol bind", static_cast<unsigned>(sym->bind()));
            return {};
        }
      }
    }

    if (!weak_def.undefined_weak() || ref.bind() == ElfSymBind::kWeak) {
      return weak_def;
    }

    diag.UndefinedSymbol(name);
    return std::nullopt;
  };
}

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_RESOLVE_H_
