// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.test.drivers.power/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>

#include <sdk/lib/sys/cpp/outgoing_directory.h>

namespace mock_power_broker {

class PowerElement {
 public:
  explicit PowerElement(fidl::ServerEnd<fuchsia_power_broker::ElementControl> ec,
                        fidl::ServerEnd<fuchsia_power_broker::Lessor> less,
                        fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> current,
                        fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> required)
      : element_control_(std::move(ec)),
        lessor_(std::move(less)),
        current_level_(std::move(current)),
        required_level_(std::move(required)) {}

 private:
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_;
  fidl::ServerEnd<fuchsia_power_broker::Lessor> lessor_;
  fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> current_level_;
  fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> required_level_;
};

class Topology : public fidl::Server<fuchsia_power_broker::Topology>,
                 public fidl::Server<fuchsia_test_drivers_power::GetPowerElements> {
 public:
  void AddElement(AddElementRequest& req, AddElementCompleter::Sync& completer) override {
    FX_LOGS(INFO) << "Got add element request for element named '"
                  << req.element_name().value().c_str() << "'";

    // Make channel to return to client
    auto element_control = fidl::CreateEndpoints<fuchsia_power_broker::ElementControl>();

    clients_.emplace_back(std::move(element_control->server),
                          std::move(req.lessor_channel().value()),
                          std::move(req.level_control_channels().value().current()),
                          std::move(req.level_control_channels().value().required()));

    fuchsia_power_broker::TopologyAddElementResponse result{
        {
            .element_control_channel = std::move(element_control->client),
        },
    };
    fit::success<fuchsia_power_broker::TopologyAddElementResponse> success(std::move(result));

    std::string element_name(req.element_name().value().data(),
                             req.element_name().value().length());
    added_elements_.emplace_back(std::move(element_name));
    if (completer_ != std::nullopt) {
      auto local = std::move(completer_.value());
      SendReply(local);
    }

    completer.Reply(std::move(success));
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  void GetElements(GetElementsCompleter::Sync& completer) override {
    if (added_elements_.size() > 0) {
      auto local = completer.ToAsync();
      SendReply(local);
      return;
    }
    completer_ = completer.ToAsync();
  }

 private:
  void SendReply(GetElementsCompleter::Async& completer) {
    fuchsia_test_drivers_power::GetPowerElementsGetElementsResponse resp;
    resp.elements() = added_elements_;
    completer.Reply(resp);
    completer_ = std::nullopt;
    added_elements_ = std::vector<std::string>();
  }
  std::optional<GetElementsCompleter::Async> completer_;
  std::vector<std::string> added_elements_;
  std::vector<PowerElement> clients_;
};

}  // namespace mock_power_broker

int main(int argc, char** argv) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  sys::OutgoingDirectory out_dir;
  loop.StartThread();

  std::vector<fidl::ServerBindingRef<fuchsia_power_broker::Topology>> topology_bindings_;
  std::vector<fidl::ServerBindingRef<fuchsia_test_drivers_power::GetPowerElements>>
      get_elements_bindings_;
  std::shared_ptr<mock_power_broker::Topology> handler =
      std::make_shared<mock_power_broker::Topology>();

  fidl::ProtocolHandler<fuchsia_power_broker::Topology> req_handler =
      [handler, &topology_bindings_, &loop](fidl::ServerEnd<fuchsia_power_broker::Topology> req) {
        fidl::ServerBindingRef<fuchsia_power_broker::Topology> ref =
            fidl::BindServer(loop.dispatcher(), std::move(req), handler,
                             [&](mock_power_broker::Topology* topo, fidl::UnbindInfo info,
                                 fidl::ServerEnd<fuchsia_power_broker::Topology> server_end) {});
        topology_bindings_.emplace_back(std::move(ref));
      };

  out_dir.AddProtocol<fuchsia_power_broker::Topology>(std::move(req_handler));
  fidl::ProtocolHandler<fuchsia_test_drivers_power::GetPowerElements> get_elements_handler =
      [handler, &get_elements_bindings_,
       &loop](fidl::ServerEnd<fuchsia_test_drivers_power::GetPowerElements> req) {
        fidl::ServerBindingRef<fuchsia_test_drivers_power::GetPowerElements> binding_ref =
            fidl::BindServer(
                loop.dispatcher(), std::move(req), handler,
                [&](mock_power_broker::Topology* t, fidl::UnbindInfo info,
                    fidl::ServerEnd<fuchsia_test_drivers_power::GetPowerElements> server_end) {});
        get_elements_bindings_.emplace_back(std::move(binding_ref));
      };

  out_dir.AddProtocol<fuchsia_test_drivers_power::GetPowerElements>(
      std::move(get_elements_handler));

  auto serve_result = out_dir.ServeFromStartupInfo(loop.dispatcher());
  if (serve_result != ZX_OK) {
    return -1;
  }

  loop.JoinThreads();
  return 0;
}
