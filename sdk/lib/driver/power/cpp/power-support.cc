// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fidl/cpp/wire/wire_messaging_declarations.h>
#include <lib/fidl/cpp/wire_natural_conversions.h>
#include <lib/fit/internal/result.h>
#include <lib/fit/result.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <lib/zx/handle.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls.h>

#include "lib/driver/power/cpp/element-description-builder.h"

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

namespace {

/// Given a `PowerDependency` extract the level dependency mappings and convert
/// them into a vector of `LevelDependency` objects. This does not set the
/// PowerDependency::token because that is not available to this function and
/// should be filled in later.
fit::result<Error, std::vector<fuchsia_power_broker::LevelDependency>> ConvertPowerDepsToLevelDeps(
    const PowerDependency& driver_config_deps) {
  std::vector<fuchsia_power_broker::LevelDependency> power_framework_deps;

  // See if this is an assertive or opportunistic dependency
  // If we don't know the type, default to assertive. This possibly results in
  // unintentionally high power consumption, but is more likely to preserve
  // programmatic correctness.
  fuchsia_power_broker::DependencyType dep_type;
  switch (driver_config_deps.strength) {
    case RequirementType::kAssertive:
      dep_type = fuchsia_power_broker::DependencyType::kAssertive;
      break;
    case RequirementType::kOpportunistic:
      dep_type = fuchsia_power_broker::DependencyType::kOpportunistic;
      break;
    default:
      if (fdf::Logger::HasGlobalInstance()) {
        FDF_LOGL(WARNING, *fdf::Logger::GlobalInstance(),
                 "Dependency level not recognized, using assertive");
      }
      dep_type = fuchsia_power_broker::DependencyType::kAssertive;
  }

  // Go through each of the level dependencies and translate them
  for (const auto& driver_framework_level_dep : driver_config_deps.level_deps) {
    fuchsia_power_broker::LevelDependency power_framework_dep;

    power_framework_dep.dependency_type() = dep_type;
    power_framework_dep.dependent_level() = driver_framework_level_dep.child_level;
    power_framework_dep.requires_level_by_preference() =
        std::vector<uint8_t>(1, driver_framework_level_dep.parent_level);
    power_framework_deps.push_back(std::move(power_framework_dep));
  }

  return fit::success(std::move(power_framework_deps));
}

/// Get the tokens for the dependencies by connecting to available endpoints in
/// |svcs_dir|. |dependencies| is consumed by this function.
///
/// Returns Error::IO if there is a problem talking to capabilities.
std::optional<Error> GetTokensFromParents(ElementDependencyMap& dependencies, TokenMap& tokens,
                                          const fidl::ClientEnd<fuchsia_io::Directory>& svcs_dir) {
  // Find our tokens from the instances we have.
  for (auto& [parent, _] : dependencies) {
    std::optional instance_name = parent.GetInstanceName();
    if (!instance_name.has_value()) {
      continue;
    }
    // Get the PowerTokenProvider for a particular service instance.
    fidl::WireSyncClient<fuchsia_hardware_power::PowerTokenProvider> token_client;
    {
      zx::result<fuchsia_hardware_power::PowerTokenService::ServiceClient> svc_instance =
          component::OpenServiceAt<fuchsia_hardware_power::PowerTokenService>(
              svcs_dir, instance_name.value());
      if (svc_instance.is_error()) {
        return Error::TOKEN_REQUEST;
      }
      zx::result<fidl::ClientEnd<fuchsia_hardware_power::PowerTokenProvider>>
          token_provider_channel = svc_instance.value().connect_token_provider();
      if (token_provider_channel.is_error()) {
        return Error::TOKEN_REQUEST;
      }
      token_client.Bind(std::move(token_provider_channel.value()));
    }

    // Phew, now that we did that, ask for the token
    fidl::WireResult<fuchsia_hardware_power::PowerTokenProvider::GetToken> token_resp =
        token_client->GetToken();
    if (!token_resp.ok() || token_resp->is_error()) {
      return Error::TOKEN_REQUEST;
    }

    fuchsia_hardware_power::wire::PowerTokenProviderGetTokenResponse* resp_val =
        token_resp->value();

    // Woohoo! We found something we depend upon, let's store it
    tokens.emplace(std::pair<ParentElement, zx::event>(parent, std::move(resp_val->handle)));
  }

  for (auto& token : tokens) {
    dependencies.erase(token.first);
  }
  return std::nullopt;
}

fit::result<Error> RegisterDependencyToken(
    fidl::UnownedClientEnd<fuchsia_power_broker::ElementControl>& element_control_client,
    const zx::unowned_event& token, const fuchsia_power_broker::DependencyType type) {
  if (!token->is_valid()) {
    return fit::error(Error::INVALID_ARGS);
  }
  zx::event dupe;
  zx_status_t dupe_result = token->duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
  if (dupe_result != ZX_OK) {
    return fit::error(Error::INVALID_ARGS);
  }

  auto result =
      fidl::WireCall(element_control_client)->RegisterDependencyToken(std::move(dupe), type);
  if (!result.ok()) {
    if (result.is_peer_closed()) {
      return fit::error(Error::IO);
    }
    // TODO(https://fxbug.dev/328266458) not sure if invalid args is right for
    // all other conditions
    return fit::error(Error::INVALID_ARGS);
  }
  return fit::success();
}

fit::result<Error> GetTokensFromCpuElementManager(
    ElementDependencyMap& dependencies, TokenMap& tokens,
    const fidl::ClientEnd<fuchsia_io::Directory>& svcs_dir) {
  // Connect to the CpuElementManager service.
  zx::result<fidl::ClientEnd<fuchsia_power_system::CpuElementManager>> cpu_elements_connect =
      component::ConnectAt<fuchsia_power_system::CpuElementManager>(svcs_dir);

  if (cpu_elements_connect.is_error()) {
    return fit::error(Error::CPU_ELEMENT_MANAGER_UNAVAILABLE);
  }

  fidl::WireSyncClient<fuchsia_power_system::CpuElementManager> cpu_elements;
  cpu_elements.Bind(std::move(cpu_elements_connect.value()));

  // Request the CPU token.
  fidl::WireResult<fuchsia_power_system::CpuElementManager::GetCpuDependencyToken>
      token_request_result = cpu_elements->GetCpuDependencyToken();
  if (!token_request_result.ok()) {
    if (token_request_result.is_peer_closed()) {
      return fit::error(Error::CPU_ELEMENT_MANAGER_UNAVAILABLE);
    }
    return fit::error(Error::CPU_ELEMENT_MANAGER_REQUEST);
  }

  zx::event cpu_token = std::move(token_request_result->assertive_dependency_token());
  if (!cpu_token.is_valid()) {
    // Should we should assert?
    return fit::error(Error::CPU_ELEMENT_MANAGER_REQUEST);
  }

  std::vector<ParentElement> found_parents = {};
  for (const auto& [parent, deps] : dependencies) {
    if (parent.type() != ParentElement::Type::kCpu) {
      continue;
    }

    for (const fuchsia_power_broker::LevelDependency& dep : deps) {
      if (dep.dependency_type() != fuchsia_power_broker::DependencyType::kAssertive) {
        return zx::error(Error::INVALID_ARGS);
      }
    }
    // Clone the token for each of the dependencies in the map that need it.
    zx::event token_copy;
    cpu_token.duplicate(ZX_RIGHT_SAME_RIGHTS, &token_copy);
    tokens.emplace(std::make_pair(parent, std::move(token_copy)));

    // Add the dep to the list we found.
    found_parents.push_back(parent);
  }

  for (const ParentElement& found : found_parents) {
    // Remove the dependency from the map so we record we found it.
    dependencies.erase(found);
  }
  return fit::success();
}

fit::result<Error> GetTokensFromSag(ElementDependencyMap& dependencies, TokenMap& tokens,
                                    const fidl::ClientEnd<fuchsia_io::Directory>& svcs_dir) {
  zx::result<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> governor_connect =
      component::ConnectAt<fuchsia_power_system::ActivityGovernor>(svcs_dir);
  if (governor_connect.is_error()) {
    return fit::error(Error::ACTIVITY_GOVERNOR_UNAVAILABLE);
  }

  fidl::WireSyncClient<fuchsia_power_system::ActivityGovernor> governor;
  governor.Bind(std::move(governor_connect.value()));
  fidl::WireResult<fuchsia_power_system::ActivityGovernor::GetPowerElements> elements =
      governor->GetPowerElements();
  if (!elements.ok()) {
    if (elements.is_peer_closed()) {
      return fit::error(Error::ACTIVITY_GOVERNOR_UNAVAILABLE);
    }
    return fit::error(Error::ACTIVITY_GOVERNOR_REQUEST);
  }

  // Track the parents we find so we can remove them from dependencies after
  // we finish iterating over the list of them
  std::vector<ParentElement> found_parents = {};

  for (const auto& [parent, deps] : dependencies) {
    if (parent.type() != ParentElement::Type::kSag) {
      continue;
    }

    // TODO(https://fxbug.dev/328527451): We should be respecting assertive vs
    // opportunistic deps here. For the very short term we know what these will
    // be for all clients, but very soon we should modify the return types and
    // return the right tokens.
    switch (parent.GetSag().value()) {
      case SagElement::kExecutionState:
        if (elements->has_execution_state() &&
            elements->execution_state().has_opportunistic_dependency_token()) {
          zx::event copy;
          elements->execution_state().opportunistic_dependency_token().duplicate(
              ZX_RIGHT_SAME_RIGHTS, &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
        break;
      case SagElement::kApplicationActivity:
        if (elements->has_application_activity() &&
            elements->application_activity().has_assertive_dependency_token()) {
          zx::event copy;
          elements->application_activity().assertive_dependency_token().duplicate(
              ZX_RIGHT_SAME_RIGHTS, &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
        break;
      default:
        return fit::error(Error::INVALID_ARGS);
    }

    // Record that we found this parent
    found_parents.push_back(parent);
  }

  // Remove the parents we found
  for (const ParentElement& found : found_parents) {
    dependencies.erase(found);
  }
  return zx::ok();
}

}  // namespace

zx::error<int> ErrorToZxError(Error e) {
  switch (e) {
    case Error::INVALID_ARGS:
      return zx::error(ZX_ERR_INVALID_ARGS);
    case Error::TOKEN_REQUEST:
    case Error::IO:
    case Error::TOPOLOGY_UNAVAILABLE:
      return zx::error(ZX_ERR_IO);
    case Error::DEPENDENCY_NOT_FOUND:
      return zx::error(ZX_ERR_NOT_FOUND);
    case Error::TOKEN_SERVICE_CAPABILITY_NOT_FOUND:
    case Error::NO_TOKEN_SERVICE_INSTANCES:
    case Error::ACTIVITY_GOVERNOR_UNAVAILABLE:
      return zx::error(ZX_ERR_ACCESS_DENIED);
    case Error::READ_INSTANCES:
    case Error::ACTIVITY_GOVERNOR_REQUEST:
      return zx::error(ZX_ERR_IO_REFUSED);
    default:
      return zx::error(ZX_ERR_INTERNAL);
  }
}

void ElementRunner::RunPowerElement() {
  // * First, we watch for a new required level.
  // * Second, we report this level to |on_level_change_|.
  // * Third, we report the level returned from |on_level_change_| which may or
  //   may not be the same value passed into |on_level_change_|.
  // If any errors occur we report this and then bail, otherwise method is
  // called again.
  required_level_client_->Watch().Then(
      [&](fidl::Result<fuchsia_power_broker::RequiredLevel::Watch>& result) {
        // The watch call result in an error, translate this to the error enum
        // accepted by `on_error_`.
        if (result.is_error()) {
          auto err = result.error_value();
          if (err.is_framework_error()) {
            if (err.framework_error().is_peer_closed()) {
              on_error_(ElementRunnerError::REQUIRED_LEVEL_TRANSPORT_PEER_CLOSED);
            } else {
              on_error_(ElementRunnerError::REQUIRED_LEVEL_TRANSPORT_OTHER);
            }
          } else {
            switch (err.domain_error()) {
              case fuchsia_power_broker::RequiredLevelError::kInternal:
                on_error_(ElementRunnerError::REQUIRED_LEVEL_INTERNAL);
                break;
              case fuchsia_power_broker::RequiredLevelError::kNotAuthorized:
                on_error_(ElementRunnerError::REQUIRED_LEVEL_NOT_AUTHORIZED);
                break;
              case fuchsia_power_broker::RequiredLevelError::kUnknown:
                on_error_(ElementRunnerError::REQUIRED_LEVEL_UNKNOWN);
                break;
              default:
                on_error_(ElementRunnerError::REQUIRED_LEVEL_UNEXPECTED);
            }
          }
          return;
        }

        required_level_.Set(result->required_level());
        fit::result<zx_status_t, uint8_t> change_result =
            on_level_change_(result->required_level());

        // The callback returned an error, report and abort.
        if (change_result.is_error()) {
          on_error_(ElementRunnerError::LEVEL_CHANGE_CALLBACK);
          return;
        }

        current_level_.Set(change_result.value());
        current_level_client_->Update({change_result.value()})
            .Then([&](fidl::Result<fuchsia_power_broker::CurrentLevel::Update>& update_result) {
              if (update_result.is_error()) {
                auto err = update_result.error_value();
                if (err.is_framework_error()) {
                  if (err.framework_error().is_peer_closed()) {
                    on_error_(ElementRunnerError::CURRENT_LEVEL_TRANSPORT_PEER_CLOSED);
                  } else {
                    on_error_(ElementRunnerError::CURRENT_LEVEL_TRANSPORT_OTHER);
                  }
                } else {
                  switch (err.domain_error()) {
                    case fuchsia_power_broker::CurrentLevelError::kNotAuthorized:
                      on_error_(ElementRunnerError::CURRENT_LEVEL_NOT_AUTHORIZED);
                      break;
                    default:
                      on_error_(ElementRunnerError::CURRENT_LEVEL_UNEXPECTED);
                  }
                }
                return;
              }

              // Call ourself again to keep running the element.
              RunPowerElement();
            });
      });
}

void ElementRunner::SetLevel(
    uint8_t level,
    fit::function<
        void(fit::result<fidl::ErrorsIn<fuchsia_power_broker::CurrentLevel::Update>, zx_status_t>)>
        callback) {
  current_level_.Set(level);

  // Sets the level and reports the result to |callback|
  current_level_client_->Update(level).Then(
      [callback = std::move(callback)](
          fidl::Result<fuchsia_power_broker::CurrentLevel::Update>& update_result) {
        if (update_result.is_error()) {
          callback(fit::error(update_result.error_value()));
        } else {
          callback(fit::ok(ZX_OK));
        }
      });
}

fit::result<Error, ElementDependencyMap> LevelDependencyFromConfig(
    const PowerElementConfiguration& element_config) {
  ElementDependencyMap element_deps{};

  // No dependencies, just return!
  if (element_config.dependencies.empty()) {
    return fit::success(std::move(element_deps));
  }

  for (const PowerDependency& power_dep : element_config.dependencies) {
    ParentElement pe = power_dep.parent;
    auto& deps_on_parent = element_deps[pe];

    // Get the dependencies between this parent and this child
    auto level_deps = ConvertPowerDepsToLevelDeps(power_dep);
    if (level_deps.is_error()) {
      return fit::error(level_deps.error_value());
    }

    // Add each level dependency to the vector associated with this parent name
    for (auto& level_dep : level_deps.value()) {
      deps_on_parent.push_back(std::move(level_dep));
    }
  }

  return fit::success(std::move(element_deps));
}

std::vector<fuchsia_power_broker::PowerLevel> PowerLevelsFromConfig(
    PowerElementConfiguration element_config) {
  std::vector<fuchsia_power_broker::PowerLevel> levels{};
  levels.reserve(element_config.element.levels.size());
  for (PowerLevel& level : element_config.element.levels) {
    levels.emplace_back(level.level);
  }

  return levels;
}

fit::result<Error, TokenMap> GetDependencyTokens(const fdf::Namespace& ns,
                                                 const PowerElementConfiguration& element_config) {
  auto [client_end, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  zx_status_t result =
      fdio_open3_at(ns.svc_dir().channel()->get(), ".",
                    static_cast<uint64_t>(fuchsia_io::wire::Flags::kProtocolDirectory),
                    server_end.channel().release());
  if (result != ZX_OK) {
    return fit::error(Error::IO);
  }
  return GetDependencyTokens(element_config, std::move(client_end));
}

fit::result<Error, TokenMap> GetDependencyTokens(const PowerElementConfiguration& element_config,
                                                 fidl::ClientEnd<fuchsia_io::Directory> svcs_dir) {
  // Why have this variant of `GetDependencyTokens`, wouldn't just taking the
  // fdf::Namespace work? Yes, except it would be hard to test because of
  // implementation details of Namespace, namely that it wraps the component
  // namespace and tries to access things from the component namespace that
  // may not be present in test scenarios.

  // Build up the list of parent names that power elements depends on
  // TODO(https://fxbug.dev/328268285) this is somewhat inefficient as what
  // we're calling does more work than we need, maybe be more efficient?
  fit::result<Error, ElementDependencyMap> dep_result = LevelDependencyFromConfig(element_config);
  if (dep_result.is_error()) {
    return fit::error(dep_result.error_value());
  }

  ElementDependencyMap dependencies = std::move(dep_result.value());
  TokenMap tokens{};

  // This power configuration has no dependencies, so no work to do!
  if (dependencies.size() == 0) {
    return zx::ok(std::move(tokens));
  }

  // Check which kind of dependencies we have
  bool have_driver_dep = false;
  bool have_sag_dep = false;
  bool have_cpu_dep = false;
  for (const auto& [parent, deps] : dependencies) {
    switch (parent.type()) {
      case ParentElement::Type::kSag:
        have_sag_dep = true;
        break;
      case ParentElement::Type::kInstanceName:
        have_driver_dep = true;
        break;
      case ParentElement::Type::kCpu:
        have_cpu_dep = true;
        break;
      default:
        return fit::error(Error::INVALID_ARGS);
    }
    if (have_driver_dep && have_sag_dep && have_cpu_dep) {
      break;
    }
  }

  if (have_driver_dep) {
    std::optional parent_tokens_error = GetTokensFromParents(dependencies, tokens, svcs_dir);
    if (parent_tokens_error.has_value()) {
      return fit::error(parent_tokens_error.value());
    }
  }

  // Deal with any system activity governor tokens we might need
  if (have_sag_dep) {
    fit::result<Error> sag_token_result = GetTokensFromSag(dependencies, tokens, svcs_dir);
    if (sag_token_result.is_error()) {
      return zx::error(sag_token_result.take_error());
    }
  }

  if (have_cpu_dep) {
    fit::result<Error> cpu_manager_result =
        GetTokensFromCpuElementManager(dependencies, tokens, svcs_dir);
    if (cpu_manager_result.is_error()) {
      return zx::error(cpu_manager_result.take_error());
    }
  }

  // Check if we found all the tokens we needed
  if (dependencies.size() > 0) {
    return fit::error(Error::DEPENDENCY_NOT_FOUND);
  }

  return zx::ok(std::move(tokens));
}

fit::result<Error> AddElement(
    const fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    const PowerElementConfiguration& config, TokenMap tokens,
    const zx::unowned_event& assertive_token, const zx::unowned_event& opportunistic_token,
    std::optional<std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
                            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>>
        level_control,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::ElementControl>> element_control,
    std::optional<fidl::UnownedClientEnd<fuchsia_power_broker::ElementControl>>
        element_control_client) {
  // Get the power levels we should have
  std::vector<fuchsia_power_broker::PowerLevel> levels = PowerLevelsFromConfig(config);
  if (levels.size() == 0) {
    return fit::error(Error::INVALID_ARGS);
  }

  // Get the level dependencies
  ElementDependencyMap dep_map;
  auto conversion_result = LevelDependencyFromConfig(config);
  if (conversion_result.is_error()) {
    return fit::error(Error::INVALID_ARGS);
  }
  dep_map = std::move(conversion_result.value());

  fidl::Arena arena;
  size_t dep_count = 0;
  // Check we have all the necessary tokens and count the number of deps so
  // we can allocate the right-sized vector to hold them
  for (const auto& [parent, dep] : dep_map) {
    if (tokens.find(parent) == tokens.end()) {
      return fit::error(Error::DEPENDENCY_NOT_FOUND);
    }

    dep_count += dep.size();
  }

  // Shove everything into a FIDL-ized structure
  std::vector<fuchsia_power_broker::LevelDependency> level_deps(dep_count);
  int dep_index = 0;
  for (const std::pair<const ParentElement, std::vector<fuchsia_power_broker::LevelDependency>>&
           dep : dep_map) {
    // Create level deps that include the dependency token
    for (const auto& needs : dep.second) {
      // TODO(https://fxbug.dev/328527451) We'll need to update this once we
      // properly handle assertive vs opportunistic tokens
      zx::event dupe;
      zx_status_t dupe_result =
          tokens.find(dep.first)->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);

      if (dupe_result != ZX_OK) {
        // This should only fail if the supplied event handle is invalid
        return fit::error(Error::INVALID_ARGS);
      }
      fuchsia_power_broker::LevelDependency c{
          {.dependency_type = needs.dependency_type(),
           .dependent_level = needs.dependent_level(),
           .requires_token = std::move(dupe),
           .requires_level_by_preference = needs.requires_level_by_preference()}};
      level_deps[dep_index] = std::move(c);
      dep_index++;
    }
  }

  std::optional<fuchsia_power_broker::LevelControlChannels> lvl_ctrl;
  if (level_control.has_value()) {
    lvl_ctrl = {std::move(level_control->first), std::move(level_control->second)};
  } else {
    level_control = std::nullopt;
  }

  fuchsia_power_broker::ElementSchema schema{{
      .element_name = std::string(config.element.name.data(), config.element.name.size()),
      .initial_current_level = static_cast<uint8_t>(0),
      .valid_levels = std::move(levels),
      .dependencies = std::move(level_deps),
      .level_control_channels = std::move(lvl_ctrl),
  }};
  if (lessor.has_value()) {
    schema.lessor_channel() = std::move(lessor.value());
  }
  if (element_control.has_value()) {
    schema.element_control() = std::move(element_control.value());
  }

  // Add the element
  auto add_result =
      fidl::WireCall(power_broker)->AddElement(fidl::ToWire(arena, std::move(schema)));
  if (!add_result.ok()) {
    if (add_result.is_peer_closed()) {
      return fit::error(Error::IO);
    }
    // TODO(https://fxbug.dev/328266458) not sure if invalid args is right for
    // all other conditions
    return fit::error(Error::INVALID_ARGS);
  }

  if (element_control_client.has_value()) {
    if (assertive_token->is_valid()) {
      fit::result<Error> assertive_result =
          RegisterDependencyToken(element_control_client.value(), assertive_token,
                                  fuchsia_power_broker::DependencyType::kAssertive);
      if (assertive_result.is_error()) {
        return assertive_result;
      }
    }
    if (opportunistic_token->is_valid()) {
      fit::result<Error> opportunistic_result =
          RegisterDependencyToken(element_control_client.value(), opportunistic_token,
                                  fuchsia_power_broker::DependencyType::kOpportunistic);
      if (opportunistic_result.is_error()) {
        return opportunistic_result;
      }
    }
  }

  return fit::success();
}

fit::result<Error> AddElement(fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
                              ElementDesc& description) {
  std::optional<fidl::UnownedClientEnd<fuchsia_power_broker::ElementControl>>
      element_control_client = std::nullopt;
  if (description.element_control_client.has_value()) {
    element_control_client =
        std::make_optional<fidl::UnownedClientEnd<fuchsia_power_broker::ElementControl>>(
            description.element_control_client->borrow());
  }
  return AddElement(power_broker, description.element_config, std::move(description.tokens),
                    description.assertive_token.borrow(), description.opportunistic_token.borrow(),
                    std::move(description.level_control_servers),
                    std::move(description.lessor_server),
                    std::move(description.element_control_server), element_control_client);
}

void LeaseHelper::AcquireLease(
    fit::function<void(fidl::Result<fuchsia_power_broker::Lessor::Lease>&)> callback) {
  lessor_->Lease({1}).Then(std::move(callback));
}

constexpr uint8_t LEVEL_OFF = 0;
constexpr uint8_t LEVEL_ON = 1;

fit::result<std::tuple<fidl::Status, std::optional<fuchsia_power_broker::AddElementError>>,
            std::unique_ptr<LeaseHelper>>
CreateLeaseHelper(const fidl::ClientEnd<fuchsia_power_broker::Topology>& topology,
                  std::vector<LeaseDependency> dependencies, std::string lease_name,
                  async_dispatcher_t* dispatcher, fit::function<void()> error_callback,
                  inspect::Node* parent) {
  // Create the channels
  fidl::Endpoints<fuchsia_power_broker::CurrentLevel> current_level =
      fidl::CreateEndpoints<fuchsia_power_broker::CurrentLevel>().value();
  fidl::Endpoints<fuchsia_power_broker::RequiredLevel> required_level =
      fidl::CreateEndpoints<fuchsia_power_broker::RequiredLevel>().value();
  fidl::Endpoints<fuchsia_power_broker::Lessor> lessor =
      fidl::CreateEndpoints<fuchsia_power_broker::Lessor>().value();
  fidl::Endpoints<fuchsia_power_broker::ElementControl> element_control =
      fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>().value();

  fuchsia_power_broker::ElementSchema element_definition;
  element_definition.element_name() = std::move(lease_name);
  element_definition.initial_current_level() = fuchsia_power_broker::PowerLevel{LEVEL_OFF};
  element_definition.valid_levels() = std::vector{fuchsia_power_broker::PowerLevel{LEVEL_OFF},
                                                  fuchsia_power_broker::PowerLevel{LEVEL_ON}};

  std::vector<fuchsia_power_broker::LevelDependency> deps(dependencies.size());
  int count = 0;
  for (auto& dep : dependencies) {
    deps[count] =
        fuchsia_power_broker::LevelDependency(dep.type, fuchsia_power_broker::PowerLevel{1},
                                              std::move(dep.token), dep.levels_by_preference);
    count++;
  }

  element_definition.dependencies() = std::move(deps);

  element_definition.level_control_channels() = fuchsia_power_broker::LevelControlChannels(
      std::move(current_level.server), std::move(required_level.server));
  element_definition.lessor_channel() = std::move(lessor.server);
  element_definition.element_control() = std::move(element_control.server);

  // Add the element
  fidl::Arena arena;
  fidl::WireResult<fuchsia_power_broker::Topology::AddElement> result =
      fidl::WireCall(topology)->AddElement(fidl::ToWire(arena, std::move(element_definition)));

  if (!result.ok()) {
    return fit::error(std::make_tuple(fidl::Status(result.error()), std::nullopt));
  }

  if (result->is_error()) {
    return fit::error(std::make_tuple(fidl::Status::Ok(), result->error_value()));
  }

  // Move the pieces from the FIDL creation result to the direct lease object
  std::unique_ptr<LeaseHelper> helper = std::make_unique<LeaseHelper>(
      element_definition.element_name().value(), std::move(element_control.client),
      std::move(lessor.client), std::move(required_level.client), std::move(current_level.client),
      dispatcher, std::move(error_callback), parent);
  return fit::success(std::move(helper));
}

fit::result<Error, std::vector<ElementDesc>> ApplyPowerConfiguration(
    const fdf::Namespace& ns, cpp20::span<PowerElementConfiguration> power_configs) {
  if (power_configs.empty()) {
    return fit::success(std::vector<ElementDesc>{});
  }

  zx::result<fidl::ClientEnd<fuchsia_power_broker::Topology>> topology_connection =
      ns.Connect<fuchsia_power_broker::Topology>();
  if (topology_connection.is_error() || !topology_connection->is_valid()) {
    return fit::error(Error::TOPOLOGY_UNAVAILABLE);
  }

  std::vector<ElementDesc> descriptions{};
  for (const PowerElementConfiguration& config : power_configs) {
    fit::result<Error, TokenMap> token_request = GetDependencyTokens(ns, config);
    if (token_request.is_error()) {
      return fit::error(token_request.error_value());
    }
    ElementDesc description = ElementDescBuilder(config, std::move(token_request.value())).Build();
    fit::result<Error> add_result = AddElement(topology_connection.value(), description);
    if (add_result.is_error()) {
      return fit::error(add_result.error_value());
    }
    descriptions.emplace_back(std::move(description));
  }
  return fit::success(std::move(descriptions));
}

}  // namespace fdf_power

#endif
