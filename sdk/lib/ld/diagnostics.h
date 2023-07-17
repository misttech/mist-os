// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_DIAGNOSTICS_H_
#define LIB_LD_DIAGNOSTICS_H_

#include <lib/elfldltl/diagnostics.h>

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

// This function gets wrapped to make a Diagnostics object.  It's responsible
// for the actual printing, which might need to use the StartupData.
//
// TODO(mcgrathr): for now, it just takes the main string. later probably wire
// up the printf engine and use PrintfDiagnosticsReport.
void ReportError(StartupData& startup, std::string_view str);

// This constructs a Report function that calls ReportError.
constexpr auto MakeDiagnosticsReport(StartupData& startup) {
  return [&startup](std::string_view str, ...) {
    ReportError(startup, str);
    return true;
  };
}

// This constructs the main Diagnostics object for the startup dynamic linker.
constexpr auto MakeDiagnostics(StartupData& startup) {
  return elfldltl::Diagnostics{MakeDiagnosticsReport(startup), StartupDiagnosticsFlags{}};
}

// Its type is captured so it can be used as an explicit non-template argument.
using Diagnostics = decltype(MakeDiagnostics(std::declval<StartupData&>()));

// This is called before proceeding from loading to relocation / linking, and
// then again when proceeding from final cleanup to transferring control.
void CheckErrors(Diagnostics& diag);

}  // namespace ld

#endif  // LIB_LD_DIAGNOSTICS_H_
