// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_
#define LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/zx/event.h>
#include <lib/zx/handle.h>

/// Collection of helpers for driver authors working with the power framework.
/// The basic usage model is
///   * use `fuchsia.hardware.platform.device/Device.GetPowerConfiguration` to
///     retrieve the config supplied by the board driver.
///   * For each power element in the driver's config
///       - Call `PowerAdapter::GetDependencyTokens` to get the element's
///         parents' access tokens.
///       - Calling `PowerAdapter::AddElement` and supplying the configuration,
///         token set from `GetDependencyTokens` and any access tokens the
///         driver needs to declare.
namespace fdf_power {

enum class Error : uint8_t {
  INVALID_ARGS,
  IO,
  DEPENDENCY_NOT_FOUND,
};

class ParentElementHasher {
 public:
  /// Make a unique string as our hash key. The string is the ordinal of the SAG
  /// value or 0 if not present followed by a forward slash, followed by the
  /// parent name, or the empty string if name is not present.
  /// [${sag}|0]/[${name}]
  size_t operator()(const fuchsia_hardware_power::ParentElement& element) const;
};

using TokenMap =
    std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event, ParentElementHasher>;

using ElementDependencyMap =
    std::unordered_map<fuchsia_hardware_power::ParentElement,
                       std::vector<fuchsia_power_broker::LevelDependency>, ParentElementHasher>;

/// Given a `PowerElementConfiguration` from driver framework, convert this
/// into a set of Power Broker's `LevelDependency` objects. The map is keyed
/// by the name of the parent/dependency.
///
/// If the `PowerElementConfiguration` expresses no dependencies, we return an
/// empty map.
///
/// NOTE: The `requires_token` of each of the `LevelDependency` objects is
/// **not** populated and must be filled in before providing this map to
/// `AddElement`.
///
/// Error returns:
///   - Error::INVALID_ARGS if `element_config` is missing fields, for example
///     if a level dependency doesn't have a parent level.
fit::result<Error, ElementDependencyMap> LevelDependencyFromConfig(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config);

/// Given a `PowerElementConfiguration` from driver framework, convert this
/// into a set of Power Broker's `PowerLevel` objects.
///
/// If the `PowerElementConfiguration` expresses no levels, we return an
/// empty vector.
std::vector<fuchsia_power_broker::PowerLevel> PowerLevelsFromConfig(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config);

/// For the Power Element represented by `element_config`, get the tokens for
/// the element's dependencies (ie. "parents") from
/// `fuchsia.hardware.power/PowerTokenProvider` instances in `ns`.
///
/// If the power element represented by `element_config` has no dependencies,
/// this function returns an empty set. If any dependency's token can not be
/// be retrieved we return an error.
/// Error returns:
///   - `Error::INVALID_ARGS` if the element_config appears invalid
///   - `Error::IO` if there is a communication failure when talking to a
///      service or a protocol required to get a token.
///   - `Error::DEPENDENCY_NOT_FOUND` if a token for a required dependency is
///     not available.
fit::result<Error, TokenMap> GetDependencyTokens(
    const fdf::Namespace& ns,
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config);

/// For the Power Element represented by `element_config`, get the tokens for
/// the
/// element's dependencies (ie. "parents") from
/// `fuchsia.hardware.power/PowerTokenProvider` instances in `svcs_dir`.
/// `svcs_dir` should contain an entry for
/// `fuchsia.hardware.power/PowerTokenService`.
///
/// Returns a set of tokens from services instances found in `svcs_dir`. If
/// the power element represented by `element_config` has no dependencies, this
/// function returns an empty set. If any dependency's token can not be
/// be retrieved we return an error.
/// Error returns:
///   - `Error::INVALID_ARGS` if the element_config appears invalid
///   - `Error::IO` if there is a communication failure when talking to a
///      service or a protocol required to get a token.
///   - `Error::DEPENDENCY_NOT_FOUND` if a token for a required dependency is
///     not available.
fit::result<Error, TokenMap> GetDependencyTokens(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config,
    fidl::ClientEnd<fuchsia_io::Directory> svcs_dir);

/// Call `AddElement` on the `power_broker` channel passed in.
/// This function uses the `config` and `tokens` arguments to properly construct
/// the call to `fuchsia.power.broker/Topology.AddElement`. Optionally callers
/// can pass in tokens to be registered for granting active and passive
/// dependency access on the created element.
///
/// Returns the response from power broker if successful which includes
/// channels to further control the created power element.
///
/// Error
///   - Error::DEPENDENCY_NOT_FOUND if there is a dependency specified by
///     `config` which is to found in `tokens`.
///   - Error::INVALID_ARGS if `config` appears to be invalid, we fail to
///     duplicate a token and therefore assume it must have been invalid, or
///     the call to power broker fails for any reason *other* than a closed
///     channel.
fit::result<Error, fuchsia_power_broker::TopologyAddElementResponse> AddElement(
    fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    fuchsia_hardware_power::wire::PowerElementConfiguration config, TokenMap tokens,
    const zx::unowned_event& active_token, const zx::unowned_event& passive_token,
    std::optional<std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
                            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>>
        level_control,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor);
}  // namespace fdf_power

#endif  // LIB_DRIVER_POWER_CPP_POWER_SUPPORT_H_
