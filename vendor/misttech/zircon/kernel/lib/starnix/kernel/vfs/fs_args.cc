// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fs_args.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/error_propagation.h>

#include <ktl/pair.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

#include <ktl/enforce.h>

namespace starnix {
namespace parse_mount_options {
namespace {

// Returns true if character is not ',' or '='
bool is_unquoted_char(char c) { return c != ',' && c != '='; }

// Parse unquoted value until ',' or '=' is found
fit::result<Errno, ktl::pair<ktl::string_view, ktl::string_view>> unquoted(ktl::string_view input) {
  size_t pos = 0;
  while (pos < input.size() && is_unquoted_char(input[pos])) {
    pos++;
  }
  if (pos == 0) {
    return fit::error(errno(EINVAL));
  }
  return fit::ok(ktl::pair(input.substr(0, pos), input.substr(pos)));
}

// Parse quoted value between double quotes
fit::result<Errno, ktl::pair<ktl::string_view, ktl::string_view>> quoted(ktl::string_view input) {
  if (input.empty() || input[0] != '"') {
    return fit::error(errno(EINVAL));
  }

  size_t end = input.find('"', 1);
  if (end == ktl::string_view::npos) {
    return fit::error(errno(EINVAL));
  }

  return fit::ok(ktl::pair(input.substr(1, end - 1), input.substr(end + 1)));
}

// Parse either quoted or unquoted value
fit::result<Errno, ktl::pair<ktl::string_view, ktl::string_view>> value(ktl::string_view input) {
  if (input.empty()) {
    return fit::error(errno(EINVAL));
  }
  if (input[0] == '"') {
    return quoted(input);
  }
  return unquoted(input);
}

// Parse key=value pair
fit::result<Errno, ktl::pair<ktl::pair<ktl::string_view, ktl::string_view>, ktl::string_view>>
key_value(ktl::string_view input) {
  auto key_result = unquoted(input) _EP(key_result);
  auto [key, rest] = key_result.value();
  if (rest.empty() || rest[0] != '=') {
    return fit::error(errno(EINVAL));
  }

  auto val_result = value(rest.substr(1)) _EP(val_result);
  auto [val, remaining] = val_result.value();
  return fit::ok(ktl::pair(ktl::pair(key, val), remaining));
}

// Parse key only (no value)
fit::result<Errno, ktl::pair<ktl::pair<ktl::string_view, ktl::string_view>, ktl::string_view>>
key_only(ktl::string_view input) {
  auto key_result = unquoted(input) _EP(key_result);
  auto [key, rest] = key_result.value();
  return fit::ok(ktl::pair(ktl::pair(key, ktl::string_view()), rest));
}

// Parse single option (either key=value or key)
fit::result<Errno, ktl::pair<ktl::pair<ktl::string_view, ktl::string_view>, ktl::string_view>>
option(ktl::string_view input) {
  auto key_value_result = key_value(input);
  if (!key_value_result.is_error()) {
    return key_value_result;
  }
  return key_only(input);
}

}  // namespace

// Main parsing function
fit::result<Errno> parse_mount_options(const FsStr& input, FsStringHashTable* out) {
  ktl::string_view remaining(input.data(), input.size());

  while (!remaining.empty()) {
    auto opt_result = option(remaining) _EP(opt_result);
    auto [kv_pair, rest] = opt_result.value();
    auto [key, value] = kv_pair;

    fbl::AllocChecker ac;
    ktl::unique_ptr<HashableFsString> hashable(new (&ac) HashableFsString{});
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }

    hashable->key = FsString(key.data(), key.size());
    hashable->value = FsString(value.data(), value.size());
    out->insert_or_replace(ktl::move(hashable));

    if (rest.empty()) {
      break;
    }

    if (rest[0] != ',') {
      return fit::error(errno(EINVAL));
    }
    remaining = rest.substr(1);
  }

  return fit::ok();
}

}  // namespace parse_mount_options

fit::result<Errno, MountParams> MountParams::parse(const FsStr& data) {
  MountParams mp;
  _EP(parse_mount_options::parse_mount_options(data, &mp.options_));
  return fit::ok(mp);
}

MountParams::MountParams(const MountParams& other) {
  for (const auto& pair : other.options_) {
    fbl::AllocChecker ac;
    ktl::unique_ptr<HashableFsString> hashable(new (&ac) HashableFsString{});
    ZX_ASSERT(ac.check());
    hashable->key = pair.key;
    hashable->value = pair.value;
    options_.insert_or_replace(ktl::move(hashable));
  }
}

}  // namespace starnix
