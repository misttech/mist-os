// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_DIAGNOSTICS_H_
#define LIB_LD_DIAGNOSTICS_H_

#include <lib/elfldltl/diagnostics.h>

#include <cassert>
#include <cstdarg>
#include <string_view>
#include <type_traits>
#include <utility>

namespace ld {

// This is declared in the OS-specific header (posix.h or zircon.h).
struct StartupData;

// This is the version of the elfldltl::DiagnosticsFlags API always used in the
// startup dynamic linker: allow warnings; keep going after errors.  Before
// beginning relocation and before handing off control, the diagnostics
// object's error count will be checked to bail out safely after doing as much
// work as possible to report all the detailed errors that can be found.
struct StartupDiagnosticsFlags {
  // Note these are not in the canonical order elfldltl::DiagnosticFlags uses.
  // No two adjacent fields have the same type for [[no_unique_address]] rules.
  [[no_unique_address]] std::false_type warnings_are_errors;
  [[no_unique_address]] std::true_type multiple_errors;
  [[no_unique_address]] std::false_type extra_checking;
};

// This provides the Report callable object for Diagnostics.  It uses the
// printf engine to ultimately call StartupMessage with the captured
// StartupData reference.  But it also holds a current module name that can
// be changed; when set, it's used as a prefix on error messages.
class DiagnosticsReport {
 public:
  constexpr explicit DiagnosticsReport(StartupData& startup) : startup_(startup) {}

  constexpr std::string_view module() const { return module_; }

  constexpr void set_module(std::string_view module) { module_ = module; }

  constexpr void clear_module() { module_ = {}; }

  template <typename... Args>
  bool operator()(Args&&... args) const {
    auto report = [&](auto... prefix) {
      auto printf = [this](auto... args) { this->Printf(args...); };
      auto report = elfldltl::PrintfDiagnosticsReport(printf, prefix...);
      report(std::forward<Args>(args)...);
    };
    if (module_.empty()) {
      report();
    } else {
      constexpr std::string_view kColon = ": ";
      report(module_, kColon);
    }
    return true;
  }

 private:
  void Printf(const char* format, ...) const;
  void Printf(const char* format, va_list args) const;

  StartupData& startup_;
  std::string_view module_;
};

// This constructs the main Diagnostics object for the startup dynamic linker.
constexpr auto MakeDiagnostics(StartupData& startup) {
  return elfldltl::Diagnostics{DiagnosticsReport(startup), StartupDiagnosticsFlags{}};
}

// Its type is captured so it can be used as an explicit non-template argument.
using Diagnostics = decltype(MakeDiagnostics(std::declval<StartupData&>()));

// This is an RAII type that exists to temporarily set the module name in the
// Diagnostics::report() object.
class ModuleDiagnostics {
 public:
  constexpr explicit ModuleDiagnostics(Diagnostics& diag, std::string_view name) : diag_(diag) {
    assert(diag_.report().module().empty());
    diag_.report().set_module(name);
  }

  ~ModuleDiagnostics() { diag_.report().clear_module(); }

 private:
  Diagnostics& diag_;
};

// This is called before proceeding from loading to relocation / linking, and
// then again when proceeding from final cleanup to transferring control.
void CheckErrors(Diagnostics& diag);

}  // namespace ld

#endif  // LIB_LD_DIAGNOSTICS_H_
