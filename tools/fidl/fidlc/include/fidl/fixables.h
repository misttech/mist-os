// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FIXABLES_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FIXABLES_H_

#include <lib/fit/function.h>

#include <map>
#include <string_view>

#include "lib/fit/result.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/raw_ast.h"
#include "tools/fidl/fidlc/include/fidl/source_manager.h"

namespace fidl {

// A list of all fixes known to fidlc. The actual fixing logic itself is implemented elsewhere, in
// the |fidl::fix| namespace. This information is intended to be exposed to the broader compiler, so
// that for instance the parser can report proper |FixableError|s.
class Fixable {
 public:
  enum struct Kind {
    kNoop,
    kProtocolModifier,
  };

  // Is this a fix that only requires successful parsing of the files-to-be-fixed, or does the fix
  // need post-compilation information as well?
  enum struct Scope {
    // We only need to parse the raw AST for the target files to perform this fix.
    kParsed,
    // The flat AST needs to be compiled as well.
    kCompiled,
  };

  constexpr Fixable(Kind kind, std::string_view name, Scope scope, ExperimentalFlags required_flags)
      : kind(kind), name(name), scope(scope), required_flags(required_flags) {}

  const Kind kind;
  const std::string_view name;
  const Scope scope;
  const ExperimentalFlags required_flags;

  // Query a statically known |Fixable| using the |Fixable::Kind|.
  static const Fixable Get(Kind kind);

  // Query a statically known |Fixable| using its string name.
  static std::optional<const Fixable> Get(const std::string& name);

  // List of active |Fixable|s.
  static std::unordered_map<const Kind, const Fixable> kActiveFixables;
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_FIXABLES_H_
