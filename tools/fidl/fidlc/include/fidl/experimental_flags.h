// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_EXPERIMENTAL_FLAGS_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_EXPERIMENTAL_FLAGS_H_

#include <lib/fit/function.h>

#include <initializer_list>
#include <map>
#include <string_view>

namespace fidl {

class ExperimentalFlags {
 public:
  using FlagSet = uint32_t;
  enum class Flag : FlagSet {
    // Used for testing, and to keep an "example experiment" if we ever have no experiments at
    // all.
    kNoop = 1 << 0,

    kAllowNewTypes = 1 << 1,
    kOutputIndexJson = 1 << 3,

    // TODO(https://fxbug.dev/110021): A temporary measure describe in
    // https://fxbug.dev/110294.
    kZxCTypes = 1 << 4,

    // These experiments control the migration to Unknown Interactions support.

    // This flag enables support for Unknown Interactions. By default, unknown
    // interactions are enabled with the legacy defaults of closed and strict.
    // TODO(https://fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractions = 1 << 7,

    // This flag enables the temporary mode where unknown interaction modifiers
    // are mandatory. This flag does not cause unknown interactions support to
    // be enabled and has no effect if the kUnknownInteractions flag is not
    // enabled.
    // TODO(https://fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractionsMandate = 1 << 8,

    // This flag changes unknown interactions to use the new defaults of open
    // and flexible. This flag does not enable unknown interactions support and
    // has no effect if the kUnknownInteractions flag is not enabled. If the
    // kUnknownInteractions mandate flag is also enabled, the defaults cannot be
    // used, so this has no effect.
    // TODO(https://fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractionsNewDefaults = 1 << 9,

    // Allow any types in error syntax, not just (u)int32 or enums thereof.
    kAllowArbitraryErrorTypes = 1 << 10,

    // Enable an allow-list for the @transitional attribute.
    kTransitionalAllowList = 1 << 11,
  };

  ExperimentalFlags() = default;
  explicit ExperimentalFlags(Flag flag) : flags_(static_cast<FlagSet>(flag)) {}
  ExperimentalFlags(std::initializer_list<Flag> flags) {
    for (const auto& flag : flags) {
      EnableFlag(flag);
    }
  }

  bool EnableFlagByName(std::string_view flag);
  void EnableFlag(Flag flag);

  bool IsFlagEnabled(Flag flag) const;
  void ForEach(const fit::function<void(const std::string_view, Flag, bool)>& fn) const;

 private:
  static std::map<const std::string_view, const Flag> kFlagStrings;

  FlagSet flags_ = 0;
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_EXPERIMENTAL_FLAGS_H_
