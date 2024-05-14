// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/graphics/drivers/msd-arm-mali/src/fuchsia_power_manager.h"
#include "src/graphics/drivers/msd-arm-mali/src/parent_device_dfv2.h"
#include "src/graphics/drivers/msd-arm-mali/tests/unit_tests/driver_logger_harness.h"

namespace {

class FakeSystemActivityGovernor : public fidl::Server<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor(zx::event exec_state_passive, zx::event wake_handling_active)
      : exec_state_passive_(std::move(exec_state_passive)),
        wake_handling_active_(std::move(wake_handling_active)) {}

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    // The wake handling element isn't actually used by the mali driver, but is included for
    // completeness and consistency with the real implementation.
    fuchsia_power_system::PowerElements elements;
    zx::event execution_element, wake_handling_element;
    exec_state_passive_.duplicate(ZX_RIGHT_SAME_RIGHTS, &execution_element);
    wake_handling_active_.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_handling_element);

    fuchsia_power_system::ExecutionState exec_state = {
        {.passive_dependency_token = std::move(execution_element)}};

    fuchsia_power_system::WakeHandling wake_handling = {
        {.active_dependency_token = std::move(wake_handling_element)}};

    elements = {
        {.execution_state = std::move(exec_state), .wake_handling = std::move(wake_handling)}};

    completer.Reply({{std::move(elements)}});
  }

  void RegisterListener(RegisterListenerRequest& req,
                        RegisterListenerCompleter::Sync& completer) override {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;

  zx::event exec_state_passive_;
  zx::event wake_handling_active_;
};

class FakeLeaseControl : public fidl::Server<fuchsia_power_broker::LeaseControl> {
 public:
  void WatchStatus(fuchsia_power_broker::LeaseControlWatchStatusRequest& req,
                   WatchStatusCompleter::Sync& completer) override {
    if (req.last_status() != lease_status_)
      completer.Reply(lease_status_);
    else {
      old_completers_.push_back(completer.ToAsync());
    }
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::LeaseControl> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  static fuchsia_power_broker::LeaseStatus lease_status_;
  std::vector<WatchStatusCompleter::Async> old_completers_;
};

fuchsia_power_broker::LeaseStatus FakeLeaseControl::lease_status_ =
    fuchsia_power_broker::LeaseStatus::kPending;

class FakeLessor : public fidl::Server<fuchsia_power_broker::Lessor> {
 public:
  void Lease(fuchsia_power_broker::LessorLeaseRequest& req,
             LeaseCompleter::Sync& completer) override {
    auto [lease_control_client_end, lease_control_server_end] =
        fidl::Endpoints<fuchsia_power_broker::LeaseControl>::Create();

    // Instantiate (fake) lease control implementation.
    auto lease_control_impl = std::make_unique<FakeLeaseControl>();
    lease_control_ = lease_control_impl.get();
    lease_control_binding_ = fidl::BindServer<fuchsia_power_broker::LeaseControl>(
        fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(lease_control_server_end),
        std::move(lease_control_impl),
        [](FakeLeaseControl* impl, fidl::UnbindInfo info,
           fidl::ServerEnd<fuchsia_power_broker::LeaseControl> server_end) mutable {});

    completer.Reply(fit::success(std::move(lease_control_client_end)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  FakeLeaseControl* lease_control_;

 private:
  std::optional<fidl::ServerBindingRef<fuchsia_power_broker::LeaseControl>> lease_control_binding_;
};

class FakeCurrentLevel : public fidl::Server<fuchsia_power_broker::CurrentLevel> {
 public:
  void AddSideEffect(
      fit::function<void(fuchsia_power_broker::PowerLevel current_level)> side_effect) {
    side_effect_ = std::move(side_effect);
  }

  void Update(fuchsia_power_broker::CurrentLevelUpdateRequest& req,
              UpdateCompleter::Sync& completer) override {
    if (side_effect_) {
      side_effect_(req.current_level());
    }
    completer.Reply(fit::success());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::CurrentLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fit::function<void(fuchsia_power_broker::PowerLevel)> side_effect_;
};

class FakeRequiredLevel : public fidl::Server<fuchsia_power_broker::RequiredLevel> {
 public:
  void Watch(WatchCompleter::Sync& completer) override {
    if (!have_replied_) {
      completer.Reply(fit::success(required_level_));
      have_replied_ = true;
    } else {
      old_completers_.push_back(completer.ToAsync());
    }
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::RequiredLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fuchsia_power_broker::PowerLevel required_level_ = 1;
  std::vector<WatchCompleter::Async> old_completers_;
  bool have_replied_ = false;
};

class PowerElement {
 public:
  explicit PowerElement(fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control,
                        fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor,
                        fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level,
                        fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level)
      : element_control_(std::move(element_control)),
        lessor_(std::move(lessor)),
        current_level_(std::move(current_level)),
        required_level_(std::move(required_level)) {}

  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_;
  fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor_;
  fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level_;
  fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level_;
};

class FakePowerBroker : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  fidl::ProtocolHandler<fuchsia_power_broker::Topology> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void AddElement(fuchsia_power_broker::ElementSchema& req,
                  AddElementCompleter::Sync& completer) override {
    // Get channels from request.
    ASSERT_TRUE(req.level_control_channels().has_value());
    fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>& current_level_server_end =
        req.level_control_channels().value().current();
    fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>& required_level_server_end =
        req.level_control_channels().value().required();
    fidl::ServerEnd<fuchsia_power_broker::Lessor>& lessor_server_end = req.lessor_channel().value();

    // Make channels to return to client
    auto [element_control_client_end, element_control_server_end] =
        fidl::Endpoints<fuchsia_power_broker::ElementControl>::Create();

    // Instantiate (fake) lessor implementation.
    auto lessor_impl = std::make_unique<FakeLessor>();
    if (req.element_name() == FuchsiaPowerManager::kHardwarePowerElementName) {
      hardware_power_lessor_ = lessor_impl.get();
    } else {
      ZX_ASSERT_MSG(0, "Unexpected power element.");
    }
    fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor_binding =
        fidl::BindServer<fuchsia_power_broker::Lessor>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(lessor_server_end),
            std::move(lessor_impl),
            [](FakeLessor* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) mutable {});

    // Instantiate (fake) current and required level implementations.
    auto current_level_impl = std::make_unique<FakeCurrentLevel>();
    auto required_level_impl = std::make_unique<FakeRequiredLevel>();
    if (req.element_name() == FuchsiaPowerManager::kHardwarePowerElementName) {
      hardware_power_current_level_ = current_level_impl.get();
      hardware_power_required_level_ = required_level_impl.get();
    } else {
      ZX_ASSERT_MSG(0, "Unexpected power element.");
    }
    fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level_binding =
        fidl::BindServer<fuchsia_power_broker::CurrentLevel>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(current_level_server_end),
            std::move(current_level_impl),
            [](FakeCurrentLevel* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> server_end) mutable {});
    fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level_binding =
        fidl::BindServer<fuchsia_power_broker::RequiredLevel>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(required_level_server_end),
            std::move(required_level_impl),
            [](FakeRequiredLevel* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> server_end) mutable {});

    servers_.emplace_back(std::move(element_control_server_end), std::move(lessor_binding),
                          std::move(current_level_binding), std::move(required_level_binding));

    fuchsia_power_broker::TopologyAddElementResponse result{
        {.element_control_channel = std::move(element_control_client_end)},
    };

    completer.Reply(fit::success(std::move(result)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  FakeLessor* hardware_power_lessor_ = nullptr;
  FakeCurrentLevel* hardware_power_current_level_ = nullptr;
  FakeRequiredLevel* hardware_power_required_level_ = nullptr;

 private:
  fidl::ServerBindingGroup<fuchsia_power_broker::Topology> bindings_;

  std::vector<PowerElement> servers_;
};

class FakePowerOwner : public FuchsiaPowerManager::Owner {
 public:
  void SetPowerState(bool enabled, PowerStateCallback completer) {
    enabled_calls_.push_back(enabled);
    completer(enabled);
  }

  std::vector<bool>& enabled_calls() { return enabled_calls_; }

 private:
  std::vector<bool> enabled_calls_;
};

struct IncomingNamespace {
  IncomingNamespace() {
    zx::event::create(0, &exec_passive);
    zx::event::create(0, &wake_active);
    zx::event exec_passive_dupe, wake_active_dupe;
    EXPECT_EQ(ZX_OK, exec_passive.duplicate(ZX_RIGHT_SAME_RIGHTS, &exec_passive_dupe));
    EXPECT_EQ(ZX_OK, wake_active.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_active_dupe));
    system_activity_governor.emplace(std::move(exec_passive_dupe), std::move(wake_active_dupe));
  }

  fdf_testing::TestNode node{"root"};
  fdf_testing::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  fake_pdev::FakePDevFidl pdev_server;
  zx::event exec_passive, wake_active;
  std::optional<FakeSystemActivityGovernor> system_activity_governor;
  FakePowerBroker power_broker;
};

fuchsia_hardware_power::PowerElementConfiguration hardware_power_config() {
  constexpr char kPowerElementName[] = "mali-gpu-hardware";

  auto transitions_from_off =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = FuchsiaPowerManager::kPoweredUpPowerLevel,
          .latency_us = 500,
      }}};
  auto transitions_from_on =
      std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
          .target_level = FuchsiaPowerManager::kPoweredDownPowerLevel,
          .latency_us = 2000,
      }}};
  fuchsia_hardware_power::PowerLevel off = {{.level = FuchsiaPowerManager::kPoweredDownPowerLevel,
                                             .name = "off",
                                             .transitions = transitions_from_off}};
  fuchsia_hardware_power::PowerLevel on = {{.level = FuchsiaPowerManager::kPoweredUpPowerLevel,
                                            .name = "on",
                                            .transitions = transitions_from_on}};
  fuchsia_hardware_power::PowerElement hardware_power = {{
      .name = kPowerElementName,
      .levels = {{off, on}},
  }};

  fuchsia_hardware_power::LevelTuple on_to_wake_handling = {{
      .child_level = FuchsiaPowerManager::kPoweredUpPowerLevel,
      .parent_level =
          static_cast<uint8_t>(fuchsia_power_system::ExecutionStateLevel::kWakeHandling),
  }};
  fuchsia_hardware_power::PowerDependency passive_on_exec_state_wake_handling = {{
      .child = kPowerElementName,
      .parent = fuchsia_hardware_power::ParentElement::WithSag(
          fuchsia_hardware_power::SagElement::kExecutionState),
      .level_deps = {{on_to_wake_handling}},
      .strength = fuchsia_hardware_power::RequirementType::kPassive,
  }};

  fuchsia_hardware_power::PowerElementConfiguration hardware_power_config = {
      {.element = hardware_power, .dependencies = {{passive_on_exec_state_wake_handling}}}};
  return hardware_power_config;
}

TEST(FuchsiaPowerManager, Basic) {
  zx::result incoming_directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  std::unique_ptr<DriverLoggerHarness> harness = DriverLoggerHarness::Create();
  fdf_testing::DriverRuntime& runtime = harness->runtime();
  fdf::UnownedSynchronizedDispatcher env_dispatcher{runtime.StartBackgroundDispatcher()};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming(
      env_dispatcher->async_dispatcher(), std::in_place);
  incoming.SyncCall([&](IncomingNamespace* incoming) mutable {
    EXPECT_TRUE(incoming->env.Initialize(std::move(incoming_directory_endpoints->server)).is_ok());
    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_irq = true;
    config.power_elements = std::vector{hardware_power_config()};
    incoming->pdev_server.SetConfig(std::move(config));
    {
      auto result =
          incoming->env.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              std::move(incoming->pdev_server.GetInstanceHandler(
                  fdf::Dispatcher::GetCurrent()->async_dispatcher())),
              "pdev");
      ASSERT_TRUE(result.is_ok());
    }
    // Serve (fake) system_activity_governor.
    {
      auto result = incoming->env.incoming_directory()
                        .component()
                        .AddUnmanagedProtocol<fuchsia_power_system::ActivityGovernor>(
                            incoming->system_activity_governor->CreateHandler());
      ASSERT_TRUE(result.is_ok());
    }

    // Serve (fake) power_broker.
    {
      auto result = incoming->env.incoming_directory()
                        .component()
                        .AddUnmanagedProtocol<fuchsia_power_broker::Topology>(
                            incoming->power_broker.CreateHandler());
      ASSERT_TRUE(result.is_ok());
    }
  });
  auto entry_incoming = fuchsia_component_runner::ComponentNamespaceEntry(
      {.path = std::string("/"), .directory = std::move(incoming_directory_endpoints->client)});
  std::vector<fuchsia_component_runner::ComponentNamespaceEntry> incoming_namespace;
  incoming_namespace.push_back(std::move(entry_incoming));

  auto fdf_incoming = fdf::Namespace::Create(incoming_namespace);
  ASSERT_TRUE(fdf_incoming.is_ok()) << fdf_incoming.status_string();
  FakePowerOwner owner;
  FuchsiaPowerManager manager(&owner);
  config::Config fake_config;
  fake_config.enable_suspend() = true;
  auto parent = ParentDeviceDFv2::Create(std::make_shared<fdf::Namespace>(std::move(*fdf_incoming)),
                                         std::move(fake_config));
  inspect::Node node;
  EXPECT_TRUE(manager.Initialize(parent.get(), node));

  runtime.RunUntil([&]() { return !owner.enabled_calls().empty(); });

  for (bool call : owner.enabled_calls()) {
    // Required power level is 1, so all calls should be to enable the GPU.
    EXPECT_TRUE(call);
  }
}

}  // namespace
