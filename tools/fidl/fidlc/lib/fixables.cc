// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/include/fidl/fixables.h"

#include <stdio.h>
#include <unistd.h>

#include <optional>

#include "lib/fit/result.h"
#include "tools/fidl/fidlc/include/fidl/experimental_flags.h"
#include "tools/fidl/fidlc/include/fidl/formatter.h"
#include "tools/fidl/fidlc/include/fidl/lexer.h"
#include "tools/fidl/fidlc/include/fidl/parser.h"
#include "tools/fidl/fidlc/include/fidl/reporter.h"

namespace fidl {

std::unordered_map<const Fixable::Kind, const Fixable> Fixable::kActiveFixables = {
    // noop
    {Fixable::Kind::kNoop,
     {Fixable::Kind::kNoop,
      "noop",
      Fixable::Scope::kParsed,
      {ExperimentalFlags(ExperimentalFlags::Flag::kNoop)}}},

    // protocol_modifier
    {Fixable::Kind::kProtocolModifier,
     {Fixable::Kind::kProtocolModifier,
      "protocol_modifier",
      Fixable::Scope::kParsed,
      {ExperimentalFlags(ExperimentalFlags::Flag::kUnknownInteractions)}}},
};

const Fixable Fixable::Get(Fixable::Kind kind) { return kActiveFixables.at(kind); }

std::optional<const Fixable> Fixable::Get(const std::string& name) {
  for (const auto& fixable : kActiveFixables) {
    if (fixable.second.name == name) {
      return fixable.second;
    }
  }
  return std::nullopt;
}

}  // namespace fidl
