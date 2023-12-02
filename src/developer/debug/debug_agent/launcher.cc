// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/launcher.h"

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.debugger/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/processargs.h>

#include <charconv>
#include <random>

namespace {

constexpr char kChildUrl[] = "fuchsia-pkg://fuchsia.com/debug_agent#meta/debug_agent.cm";
constexpr char kCollectionName[] = "agents";
constexpr char kAgentPrefix[] = "agent-";

// Convert various result types to zx_status_t.
template <typename ResultType>
zx_status_t ResultToStatus(const ResultType& result) {
  zx_status_t error = ZX_OK;
  if (result.is_error()) {
    if (result.error_value().is_domain_error()) {
      error = fidl::ToUnderlying(result.error_value().domain_error());
    } else {
      error = result.error_value().framework_error().status();
    }
  }
  return error;
}

std::string GetRandomHexString() {
  std::mt19937 prng{std::random_device{}()};
  uint32_t num = prng();

  std::string s;
  s.resize(8);
  auto [end, ec] = std::to_chars(s.data(), s.data() + s.size(), num, 16);
  FX_CHECK(ec == std::errc()) << std::make_error_code(ec).message();

  return std::string{s.data(), end};
}

}  // namespace

void DebugAgentLauncher::Launch(LaunchRequest& request, LaunchCompleter::Sync& completer) {
  std::string name(kAgentPrefix);
  name += GetRandomHexString();

  LaunchDebugAgent(std::move(request.agent()), name,
                   [completer = completer.ToAsync()](zx_status_t status) mutable {
                     completer.Reply(zx::make_result(status));
                   });
}

void DebugAgentLauncher::LaunchDebugAgent(fidl::ServerEnd<fuchsia_debugger::DebugAgent> server_end,
                                          const std::string& name,
                                          fit::callback<void(zx_status_t)> cb) {
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  fidl::Client<fuchsia_component::Realm> realm(std::move(*client_end),
                                               async_get_default_dispatcher());

  auto [controller_client_end, controller_server_end] =
      *fidl::CreateEndpoints<fuchsia_component::Controller>();

  fidl::Client<fuchsia_component::Controller> controller(std::move(controller_client_end),
                                                         async_get_default_dispatcher());

  // Create the child decl and all the starting parameters.
  fuchsia_component_decl::Child child_decl;
  child_decl.name(name);
  child_decl.url(kChildUrl);
  child_decl.startup(fuchsia_component_decl::StartupMode::kLazy);

  fuchsia_component_decl::CollectionRef collection(kCollectionName);

  std::vector<fuchsia_process::HandleInfo> handles;
  handles.emplace_back(server_end.TakeHandle(), PA_HND(PA_USER0, 0));

  fuchsia_component::CreateChildArgs args;
  args.numbered_handles() = std::move(handles);
  args.controller(std::move(controller_server_end));

  // Create and Start the child.
  realm->CreateChild({collection, child_decl, std::move(args)})
      .Then([realm = std::move(realm), controller = std::move(controller), cb = std::move(cb)](
                fidl::Result<fuchsia_component::Realm::CreateChild>& result) mutable {
        if (auto status = ResultToStatus(result); status != ZX_OK) {
          return cb(status);
        }

        controller->IsStarted().Then(
            [controller = std::move(controller), cb = std::move(cb)](
                fidl::Result<fuchsia_component::Controller::IsStarted>& result) mutable {
              if (auto status = ResultToStatus(result); status != ZX_OK) {
                return cb(status);
              }

              // If the component is already started, then we're done. This is
              // not guaranteed by the Realm protocol.
              if (result.value().is_started()) {
                return cb(ZX_OK);
              }

              // Otherwise, we need to explicitly start the child.
              auto [exec_controller_client, exec_controller_server] =
                  *fidl::CreateEndpoints<fuchsia_component::ExecutionController>();
              fidl::Client<fuchsia_component::ExecutionController> execution_controller(
                  std::move(exec_controller_client), async_get_default_dispatcher());

              controller->Start({{.execution_controller = std::move(exec_controller_server)}})
                  .Then([controller = std::move(controller), cb = std::move(cb)](
                            fidl::Result<fuchsia_component::Controller::Start>& result) mutable {
                    if (auto status = ResultToStatus(result); status != ZX_OK) {
                      return cb(status);
                    }

                    cb(ZX_OK);
                  });
            });
      });
}
