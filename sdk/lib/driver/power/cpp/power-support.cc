// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power-support.h"

#include <dirent.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
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
#include <zircon/system/public/zircon/errors.h>

namespace fdf_power {

namespace {
// Based on `dirent` as defined in
// https://cs.opensource.google/fuchsia/fuchsia/+/9b3b47f13c3c8b41c4a00fe4a2d2a2688b9a807c:sdk/fidl/fuchsia.io/directory.fidl
constexpr size_t kInoSize = 8;
constexpr size_t kTypeSize = 1;
constexpr size_t kNameSizeSize = 1;

/// Given a `PowerDependency` extract the level dependency mappings and convert
/// them into a vector of `LevelDependency` objects. This does not set the
/// PowerDependency::token because that is not available to this function and
/// should be filled in later.
fit::result<Error, std::vector<fuchsia_power_broker::LevelDependency>> ConvertPowerDepsToLevelDeps(
    fuchsia_hardware_power::wire::PowerDependency* driver_config_deps) {
  std::vector<fuchsia_power_broker::LevelDependency> power_framework_deps;

  // Some of the required fields are missing
  if (!driver_config_deps->has_strength() || !driver_config_deps->has_level_deps()) {
    return fit::error(Error::INVALID_ARGS);
  }

  // See if this is an active or passive dependency
  fuchsia_power_broker::DependencyType dep_type;
  switch (driver_config_deps->strength()) {
    case fuchsia_hardware_power::wire::RequirementType::kActive:
      dep_type = fuchsia_power_broker::DependencyType::kActive;
      break;
    case fuchsia_hardware_power::wire::RequirementType::kPassive:
      dep_type = fuchsia_power_broker::DependencyType::kPassive;
      break;
  }

  // Go through each of the level dependencies and translate them
  for (auto& driver_framework_level_dep : driver_config_deps->level_deps()) {
    fuchsia_power_broker::LevelDependency power_framework_dep;
    if (!driver_framework_level_dep.has_child_level() ||
        !driver_framework_level_dep.has_parent_level()) {
      return fit::error(Error::INVALID_ARGS);
    }

    power_framework_dep.dependency_type() = dep_type;
    power_framework_dep.dependent_level() = driver_framework_level_dep.child_level();
    power_framework_dep.requires_level() = driver_framework_level_dep.parent_level();
    power_framework_deps.push_back(std::move(power_framework_dep));
  }

  return fit::success(std::move(power_framework_deps));
}

/// Get the tokens for the dependencies by connecting to available endpoints in
/// |svcs_dir|. |dependencies| is consumed by this function.
///
/// Returns Error::IO if there is a problem talking to capabilities.
fit::result<Error, bool> GetTokensFromParents(ElementDependencyMap& dependencies, TokenMap& tokens,
                                              fidl::ClientEnd<fuchsia_io::Directory>& svcs_dir) {
  const uint64_t dir_read_page_size = static_cast<const uint64_t>(8 * 1024);

  std::vector<std::string> service_instances;
  fidl::WireSyncClient<fuchsia_io::Directory> svcs_dir_client(std::move(svcs_dir));

  // Enumerate the list os service instances.
  {
    fidl::Endpoints<fuchsia_io::Node> svc_instance_endpoints =
        fidl::CreateEndpoints<fuchsia_io::Node>().value();

    // Open the directory containing the services instances
    fidl::OneWayStatus status = svcs_dir_client->Open(
        ::fuchsia_io::wire::OpenFlags::kDirectory, ::fuchsia_io::wire::ModeType::kDoNotUse,
        fuchsia_hardware_power::PowerTokenService::Name, std::move(svc_instance_endpoints.server));

    if (!status.ok()) {
      return fit::error(Error::IO);
    }

    fidl::WireSyncClient<fuchsia_io::Directory> svcs(
        fidl::ClientEnd<fuchsia_io::Directory>(svc_instance_endpoints.client.TakeChannel()));

    // TODO(https://fxbug.dev/328630967) Check if there are more granular errors we should return
    fidl::WireResult<fuchsia_io::Directory::ReadDirents> read_result =
        svcs->ReadDirents(dir_read_page_size);

    // Peer closed is what we get if the service directory doesn't exist, we
    // consider this okay
    if (read_result.is_peer_closed()) {
      return fit::error(Error::IO);
    }

    if (read_result.status() != ZX_OK) {
      // Some non-closed error happened indicating the service directory
      // existed, but we couldn't access it
      return fit::error(Error::IO);
    }

    // Build up the list of parent names which we can then match against parent
    // power elements as expressed in the configuration.
    auto contents = fidl::ToNatural(read_result.value()).dirents();
    uint16_t offset = 0;
    while (offset < contents.size()) {
      // TODO(https://fxbug.dev/328660976) Maybe read into a struct instead of
      // byte parsing.
      // We don't care about the inode
      offset += kInoSize;
      uint8_t name_len = contents[offset];
      // Read the length of the name, so move past it
      offset += kNameSizeSize;
      // We don't care about the type
      offset += kTypeSize;
      std::string name = std::string(&contents[offset], &contents[offset + name_len]);

      // Make sure this isn't just the entry to the directory itself
      if (std::string(".") != name) {
        service_instances.emplace_back(std::move(name));
      }

      offset += name_len;
    }

    // Take back the client end so we can use it later.
    svcs_dir = svcs_dir_client.TakeClientEnd();
  }

  // Go through the service instances we have and ask for a token. For all
  // GetToken calls that return a name in our list of parent names we need to
  // insert that token in the map under the name provided by the parent
  for (const auto& instance : service_instances) {
    // Get the PowerTokenProvider for a particular service instance.
    fidl::WireSyncClient<fuchsia_hardware_power::PowerTokenProvider> token_client;
    {
      zx::result<fuchsia_hardware_power::PowerTokenService::ServiceClient> svc_instance =
          component::OpenServiceAt<fuchsia_hardware_power::PowerTokenService>(svcs_dir, instance);
      if (svc_instance.is_error()) {
        return fit::error(Error::IO);
      }
      zx::result<fidl::ClientEnd<fuchsia_hardware_power::PowerTokenProvider>>
          token_provider_channel = svc_instance.value().connect_token_provider();
      if (token_provider_channel.is_error()) {
        return fit::error(Error::IO);
      }
      token_client.Bind(std::move(token_provider_channel.value()));
    }

    // Phew, now that we did that, ask for the token
    fidl::WireResult<fuchsia_hardware_power::PowerTokenProvider::GetToken> token_resp =
        token_client->GetToken();
    if (!token_resp.ok() || token_resp->is_error()) {
      // This call failed, but maybe others will succeed?
      return fit::error(Error::IO);
    }

    fuchsia_hardware_power::wire::PowerTokenProviderGetTokenResponse* resp_val =
        token_resp->value();
    fuchsia_hardware_power::ParentElement parent = fuchsia_hardware_power::ParentElement::WithName(
        std::string(resp_val->name.data(), resp_val->name.size()));

    // Check that we depend on this element per our configuration
    if (dependencies.find(parent) == dependencies.end()) {
      // Seems like we don't depend on this provider, oh well, let's keep looking.
      continue;
    }

    // Woohoo! We found something we depend upon, let's store it
    tokens.emplace(std::pair<fuchsia_hardware_power::ParentElement, zx::event>(
        parent, std::move(resp_val->handle)));

    // Check this dependency off by removing it from the set of ones we need
    dependencies.erase(parent);
  }

  return fit::success(true);
}

}  // namespace

size_t ParentElementHasher::operator()(const fuchsia_hardware_power::ParentElement& element) const {
  switch (element.Which()) {
    case fuchsia_hardware_power::ParentElement::Tag::kSag: {
      return std::hash<std::string>{}(std::to_string(static_cast<uint32_t>(element.sag().value())) +
                                      "/");
    } break;
    case fuchsia_hardware_power::ParentElement::Tag::kName: {
      return std::hash<std::string>{}(
          std::to_string(static_cast<uint32_t>(0)) + "/" +
          std::string(element.name()->data(), element.name()->length()));

    } break;
  }
}

fit::result<Error, ElementDependencyMap> LevelDependencyFromConfig(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config) {
  ElementDependencyMap level_deps{};

  // No dependencies, just return!
  if (!element_config.has_dependencies()) {
    return fit::success(std::move(level_deps));
  }

  fidl::VectorView<::fuchsia_hardware_power::wire::PowerDependency>& deps =
      element_config.dependencies();
  for (auto& power_dep : deps) {
    fuchsia_hardware_power::ParentElement pe = fidl::ToNatural(power_dep.parent());
    auto& deps_on_parent = level_deps[pe];

    // Get the dependencies between this parent and this child
    auto dep = ConvertPowerDepsToLevelDeps(&power_dep);
    if (dep.is_error()) {
      return fit::error(dep.error_value());
    }

    // Add each level dependency to the vector associated with this parent name
    for (auto& dep : dep.value()) {
      deps_on_parent.push_back(std::move(dep));
    }
  }

  return fit::success(std::move(level_deps));
}

std::vector<fuchsia_power_broker::PowerLevel> PowerLevelsFromConfig(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config) {
  std::vector<fuchsia_power_broker::PowerLevel> levels{};

  if (!element_config.has_element() || !element_config.element().has_levels()) {
    return levels;
  }

  for (fuchsia_hardware_power::wire::PowerLevel& level : element_config.element().levels()) {
    if (!level.has_level()) {
      continue;
    }
    levels.emplace_back(level.level());
  }

  return levels;
}

fit::result<Error, TokenMap> GetDependencyTokens(
    const fdf::Namespace& ns,
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config) {
  fidl::Endpoints<fuchsia_io::Directory> svc_instance_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>().value();
  zx_status_t result = fdio_open_at(ns.svc_dir().channel()->get(), ".",
                                    static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDirectory),
                                    svc_instance_endpoints.server.channel().release());
  if (result != ZX_OK) {
    return fit::error(Error::IO);
  }
  return GetDependencyTokens(element_config, std::move(svc_instance_endpoints.client));
}

fit::result<Error, TokenMap> GetDependencyTokens(
    fuchsia_hardware_power::wire::PowerElementConfiguration element_config,
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
  for (const auto& [parent, deps] : dependencies) {
    switch (parent.Which()) {
      case fuchsia_hardware_power::ParentElement::Tag::kName:
        have_driver_dep = true;
        break;
      case fuchsia_hardware_power::ParentElement::Tag::kSag:
        have_sag_dep = true;
        break;
    }
    if (have_driver_dep && have_sag_dep) {
      break;
    }
  }

  if (have_driver_dep) {
    auto parent_tokens_result = GetTokensFromParents(dependencies, tokens, svcs_dir);
    if (parent_tokens_result.is_error()) {
      return fit::error(parent_tokens_result.take_error());
    }
  }

  if (!have_sag_dep) {
    return zx::ok(std::move(tokens));
  }

  // Deal with any system activity governor tokens we might need
  zx::result<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> governor_connect =
      component::ConnectAt<fuchsia_power_system::ActivityGovernor>(svcs_dir);
  if (governor_connect.is_error()) {
    return fit::error(Error::IO);
  }
  fidl::WireSyncClient<fuchsia_power_system::ActivityGovernor> governor;
  governor.Bind(std::move(governor_connect.value()));

  fidl::WireResult elements = governor->GetPowerElements();
  if (!elements.ok()) {
    return fit::error(Error::IO);
  }

  // Track the parents we find so we can remove them from dependencies after
  // we finish iterating over the list of them
  std::vector<fuchsia_hardware_power::ParentElement> found_parents = {};

  for (const auto& [parent, deps] : dependencies) {
    if (parent.Which() == fuchsia_hardware_power::ParentElement::Tag::kName) {
      continue;
    }

    // TODO(https://fxbug.dev/328527451): We should be respecting active vs
    // passive deps here. For the very short term we know what these will be
    // for all clients, but very soon we should modify the return types and
    // return the right tokens.
    switch (parent.sag().value()) {
      case fuchsia_hardware_power::SagElement::kExecutionState: {
        if (elements->has_execution_state() &&
            elements->execution_state().has_passive_dependency_token()) {
          zx::event copy;
          elements->execution_state().passive_dependency_token().duplicate(ZX_RIGHT_SAME_RIGHTS,
                                                                           &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
      } break;
      case fuchsia_hardware_power::SagElement::kExecutionResumeLatency: {
        if (elements->has_execution_resume_latency() &&
            elements->execution_resume_latency().has_active_dependency_token()) {
          zx::event copy;
          elements->execution_resume_latency().active_dependency_token().duplicate(
              ZX_RIGHT_SAME_RIGHTS, &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
      } break;
      case fuchsia_hardware_power::SagElement::kWakeHandling: {
        if (elements->has_wake_handling() &&
            elements->wake_handling().has_active_dependency_token()) {
          zx::event copy;
          elements->wake_handling().active_dependency_token().duplicate(ZX_RIGHT_SAME_RIGHTS,
                                                                        &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
      } break;
      case fuchsia_hardware_power::SagElement::kApplicationActivity: {
        if (elements->has_application_activity() &&
            elements->application_activity().has_active_dependency_token()) {
          zx::event copy;
          elements->application_activity().active_dependency_token().duplicate(ZX_RIGHT_SAME_RIGHTS,
                                                                               &copy);
          tokens.emplace(std::make_pair(parent, std::move(copy)));
        } else {
          return fit::error(Error::DEPENDENCY_NOT_FOUND);
        }
      } break;
    }

    // Record that we found this parent
    found_parents.push_back(parent);
  }

  // Remove the parents we found and check if we found them all
  for (const fuchsia_hardware_power::ParentElement& found : found_parents) {
    dependencies.erase(found);
  }
  if (dependencies.size() > 0) {
    return fit::error(Error::DEPENDENCY_NOT_FOUND);
  }

  return zx::ok(std::move(tokens));
}

fit::result<Error, fuchsia_power_broker::TopologyAddElementResponse> AddElement(
    fidl::ClientEnd<fuchsia_power_broker::Topology>& power_broker,
    fuchsia_hardware_power::wire::PowerElementConfiguration config, TokenMap tokens,
    const zx::unowned_event& active_token, const zx::unowned_event& passive_token,
    std::optional<std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
                            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>>
        level_control,
    std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor) {
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
  for (const std::pair<const fuchsia_hardware_power::ParentElement,
                       std::vector<fuchsia_power_broker::LevelDependency>>& dep : dep_map) {
    // Create level deps that include the dependency token
    for (const auto& needs : dep.second) {
      // TODO(https://fxbug.dev/328527451) We'll need to update this once we
      // properly handle active vs passive tokens
      zx::event dupe;
      zx_status_t dupe_result =
          tokens.find(dep.first)->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);

      if (dupe_result != ZX_OK) {
        // This should only fail if the supplied event handle is invalid
        return fit::error(Error::INVALID_ARGS);
      }
      fuchsia_power_broker::LevelDependency c{{.dependency_type = needs.dependency_type(),
                                               .dependent_level = needs.dependent_level(),
                                               .requires_token = std::move(dupe),
                                               .requires_level = needs.requires_level()}};
      level_deps[dep_index] = std::move(c);
      dep_index++;
    }
  }

  // Duplicate the token for active dependencies
  std::vector<zx::event> active_tokens{};
  if (active_token->is_valid()) {
    zx::event dupe;
    zx_status_t dupe_result = active_token->duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
    if (dupe_result != ZX_OK) {
      return fit::error(Error::INVALID_ARGS);
    }
    active_tokens.emplace_back(std::move(dupe));
  }

  // Duplicate the token for passive dependencies
  std::vector<zx::event> passive_tokens{};
  if (passive_token->is_valid()) {
    zx::event dupe;
    zx_status_t dupe_result = passive_token->duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
    if (dupe_result != ZX_OK) {
      return fit::error(Error::INVALID_ARGS);
    }
    passive_tokens.emplace_back(std::move(dupe));
  }

  std::optional<fuchsia_power_broker::LevelControlChannels> lvl_ctrl;
  if (level_control.has_value()) {
    lvl_ctrl = {std::move(level_control->first), std::move(level_control->second)};
  } else {
    level_control = std::nullopt;
  }

  fuchsia_power_broker::ElementSchema schema{{
      .element_name = std::string(config.element().name().data(), config.element().name().size()),
      .initial_current_level = static_cast<uint8_t>(0),
      .valid_levels = std::move(levels),
      .dependencies = std::move(level_deps),
      .active_dependency_tokens_to_register = std::move(active_tokens),
      .passive_dependency_tokens_to_register = std::move(passive_tokens),
      .level_control_channels = std::move(lvl_ctrl),
  }};
  if (lessor.has_value()) {
    schema.lessor_channel() = std::move(lessor.value());
  }

  // Steal the underlying channel
  fidl::WireSyncClient<fuchsia_power_broker::Topology> pb(
      fidl::ClientEnd<fuchsia_power_broker::Topology>(zx::channel(power_broker.TakeChannel())));

  // Add the element
  auto add_result = pb->AddElement(fidl::ToWire(arena, std::move(schema)));

  // Put the channel back where it belongs
  power_broker.reset(pb.TakeClientEnd().TakeChannel().release());

  if (!add_result.ok()) {
    if (add_result.is_peer_closed()) {
      return fit::error(Error::IO);
    }
    // TODO(https://fxbug.dev/328266458) not sure if invalid args is right for
    // all other conditions
    return fit::error(Error::INVALID_ARGS);
  }

  return fit::success(fidl::ToNatural(std::move(*add_result->value())));
}

}  // namespace fdf_power
