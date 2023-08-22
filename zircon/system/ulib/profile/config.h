// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_PROFILE_CONFIG_H_
#define ZIRCON_SYSTEM_ULIB_PROFILE_CONFIG_H_

#include <lib/fit/result.h>
#include <lib/zx/profile.h>
#include <lib/zx/time.h>
#include <zircon/syscalls.h>

#include <functional>
#include <map>
#include <string>
#include <string_view>
#include <unordered_map>

#include <fbl/enum_bits.h>

namespace zircon_profile {

enum class ProfileScope {
  None = 0,
  Bringup,
  Core,
  Product,
  Builtin,
};
FBL_ENABLE_ENUM_BITS(ProfileScope)

struct Profile {
  ProfileScope scope{ProfileScope::None};
  zx_profile_info_t info{};
  zx::profile profile{};
};

// Avoid unnecessary std::string temporaries during map lookups when compiling with C++20.
struct StringHash {
  using HashType = std::hash<std::string_view>;
  using is_transparent = void;

  size_t operator()(const char* string) const { return HashType{}(string); }
  size_t operator()(const std::string& string) const { return HashType{}(string); }
  size_t operator()(const std::string_view& string) const { return HashType{}(string); }
};

using ProfileMap = std::unordered_map<std::string, Profile, StringHash, std::equal_to<>>;

fit::result<std::string, ProfileMap> LoadConfigs(const std::string& config_path);

struct Role {
  std::string name;
  // Use std::less<> allow find to deduce the key type and avoid std::string temporaries.
  std::map<std::string, std::string, std::less<>> selectors;

  bool has(std::string_view key) const { return selectors.find(key) != selectors.end(); }
};
fit::result<fit::failed, Role> ParseRoleSelector(std::string_view role_selector);

struct MediaRole {
  zx_duration_t capacity;
  zx_duration_t deadline;
};
fit::result<fit::failed, MediaRole> MaybeMediaRole(const Role& role);

}  // namespace zircon_profile

#endif  // ZIRCON_SYSTEM_ULIB_PROFILE_CONFIG_H_
