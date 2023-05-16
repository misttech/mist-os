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
    kNoOptionalStructs = 1 << 2,
    kOutputIndexJson = 1 << 3,

    // TODO(fxbug.dev/110021): A temporary measure describe in
    // fxbug.dev/110294.
    kZxCTypes = 1 << 4,

    // TODO(fxbug.dev/112767): Remove once soft transition is done.
    kSimpleEmptyResponseSyntax = 1 << 5,

    // TODO(fxbug.dev/100478): Allows backends to implement overflowing experiments.
    kAllowOverflowing = 1 << 6,

    // These experiments control the migration to Unknown Interactions support.

    // This flag enables support for Unknown Interactions. By default, unknown
    // interactions are enabled with the legacy defaults of closed and strict.
    // TODO(fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractions = 1 << 7,

    // This flag enables the temporary mode where unknown interaction modifiers
    // are mandatory. This flag does not cause unknown interactions support to
    // be enabled and has no effect if the kUnknownInteractions flag is not
    // enabled.
    // TODO(fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractionsMandate = 1 << 8,

    // This flag changes unknown interactions to use the new defaults of open
    // and flexible. This flag does not enable unknown interactions support and
    // has no effect if the kUnknownInteractions flag is not enabled. If the
    // kUnknownInteractions mandate flag is also enabled, the defaults cannot be
    // used, so this has no effect.
    // TODO(fxbug.dev/88366): Remove once unknown interactions are supported.
    kUnknownInteractionsNewDefaults = 1 << 9,
  };

  ExperimentalFlags() = default;
  explicit ExperimentalFlags(Flag flag) : flags_(static_cast<FlagSet>(flag)) {}
  explicit ExperimentalFlags(std::initializer_list<Flag> flags) {
    for (const auto& flag : flags) {
      EnableFlag(flag);
    }
  }

  bool EnableFlagByName(std::string_view flag);
  void EnableFlag(Flag flag);

  bool IsFlagEnabled(Flag flag) const;
  void ForEach(const fit::function<void(const std::string_view, Flag, bool)>& fn) const;

 private:
  static std::map<const std::string_view, const Flag> FLAG_STRINGS;

  FlagSet flags_{0};
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_EXPERIMENTAL_FLAGS_H_
