// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/testing/ui_test_realm/ui_test_realm.h"

#include <fuchsia/accessibility/cpp/fidl.h>
#include <fuchsia/accessibility/scene/cpp/fidl.h>
#include <fuchsia/accessibility/semantics/cpp/fidl.h>
#include <fuchsia/input/injection/cpp/fidl.h>
#include <fuchsia/input/virtualkeyboard/cpp/fidl.h>
#include <fuchsia/intl/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <fuchsia/scheduler/cpp/fidl.h>
#include <fuchsia/session/scene/cpp/fidl.h>
#include <fuchsia/settings/cpp/fidl.h>
#include <fuchsia/sys/cpp/fidl.h>
#include <fuchsia/tracing/provider/cpp/fidl.h>
#include <fuchsia/ui/accessibility/view/cpp/fidl.h>
#include <fuchsia/ui/app/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/focus/cpp/fidl.h>
#include <fuchsia/ui/input/cpp/fidl.h>
#include <fuchsia/ui/observation/scope/cpp/fidl.h>
#include <fuchsia/ui/observation/test/cpp/fidl.h>
#include <fuchsia/ui/pointer/augment/cpp/fidl.h>
#include <fuchsia/ui/pointerinjector/configuration/cpp/fidl.h>
#include <fuchsia/ui/pointerinjector/cpp/fidl.h>
#include <fuchsia/ui/policy/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <fuchsia/ui/views/cpp/fidl.h>
#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <math.h>

#include <test/accessibility/cpp/fidl.h>
#include <test/inputsynthesis/cpp/fidl.h>

#include "sdk/lib/syslog/cpp/macros.h"

namespace ui_testing {

namespace {

using component_testing::Capability;
using component_testing::ChildRef;
using component_testing::ConfigValue;
using component_testing::Directory;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::Ref;
using component_testing::Route;
using component_testing::Storage;

// TODO(fxbug.dev/114001): Remove hard-coded values.
// Fake display dimensions.
constexpr auto kDisplayWidthPhysicalPixels = 1280;
constexpr auto kDisplayHeightPhysicalPixels = 800;

// Pixel density + usage that result in a DPR of 1.
constexpr float kLowResolutionDisplayPixelDensity = 4.1668f;
constexpr auto kDisplayUsageNear = "near";

// Base realm urls.
constexpr auto kScenicOnlyUrl = "#meta/scenic_only.cm";
constexpr auto kSceneManagerSceneUrl = "#meta/scene_manager_scene.cm";

// System component urls.
constexpr auto kRealA11yManagerUrl = "#meta/a11y-manager.cm";
constexpr auto kFakeA11yManagerUrl = "#meta/fake-a11y-manager.cm";

constexpr auto kClientSubrealmName = "client-subrealm";

constexpr auto kIntlUrl = "#meta/intl_property_manager.cm";
constexpr auto kSetUIAccessibilityUrl = "#meta/setui_accessibility.cm";

// Component names.
// NOTE: These names must match the names in meta/*.cml.
constexpr auto kA11yManagerName = "a11y-manager";
constexpr auto kScenicName = "scenic";
constexpr auto kSceneManagerName = "scene_manager";
constexpr auto kTextManagerName = "text_manager";
constexpr auto kVirtualKeyboardManagerName = "virtual_keyboard_manager";
constexpr auto kSceneProviderName = "scene-provider";
constexpr auto kSetUIAccessibility = "setui";
constexpr auto kIntl = "intl";

// Set of low-level system services that components in the realm can consume
// from parent (test_manager).
std::vector<std::string> DefaultSystemServices() {
  return {fuchsia::logger::LogSink::Name_, fuchsia::scheduler::ProfileProvider::Name_,
          fuchsia::sysmem::Allocator::Name_, fuchsia::tracing::provider::Registry::Name_,
          fuchsia::vulkan::loader::Loader::Name_};
}

// List of scenic services available in the test realm.
std::vector<std::string> ScenicServices(const UITestRealm::Config& config) {
  if (config.use_flatland) {
    // Note that we expose FlatlandDisplay to the client subrealm for now, since
    // we only have in-tree test clients at the moment. Once UITestManager is
    // used for out-of-tree tests, we'll want to add a flag to
    // UITestRealm::Config to control whether we expose internal-only APIs to
    // the client subrealm.
    return {fuchsia::ui::observation::test::Registry::Name_,
            fuchsia::ui::observation::scope::Registry::Name_,
            fuchsia::ui::pointer::augment::LocalHit::Name_,
            fuchsia::ui::composition::Allocator::Name_,
            fuchsia::ui::composition::Flatland::Name_,
            fuchsia::ui::composition::FlatlandDisplay::Name_,
            fuchsia::ui::scenic::Scenic::Name_};
  } else {
    return {fuchsia::ui::observation::test::Registry::Name_,
            fuchsia::ui::observation::scope::Registry::Name_,
            fuchsia::ui::pointer::augment::LocalHit::Name_,
            fuchsia::ui::focus::FocusChainListenerRegistry::Name_,
            fuchsia::ui::scenic::Scenic::Name_,
            fuchsia::ui::views::ViewRefInstalled::Name_};
  }
}

// List of a11y services available in the test realm.
std::vector<std::string> AccessibilityServices(const UITestRealm::Config& config) {
  if (!config.accessibility_owner) {
    return {};
  }

  return {fuchsia::accessibility::semantics::SemanticsManager::Name_,
          fuchsia::accessibility::ColorTransform::Name_, fuchsia::accessibility::Magnifier::Name_};
}

// List of scene owner services available in the test realm.
std::vector<std::string> SceneOwnerServices(const UITestRealm::Config& config) {
  if (!config.use_scene_owner) {
    return {fuchsia::ui::pointerinjector::Registry::Name_};
  }

  return {fuchsia::session::scene::Manager::Name_,
          fuchsia::ui::accessibility::view::Registry::Name_,
          fuchsia::input::injection::InputDeviceRegistry::Name_,
          fuchsia::ui::policy::DeviceListenerRegistry::Name_};
}

// Returns a mapping from ui service name to the component that vends the
// service.
std::map<std::string, std::string> GetServiceToComponentMap(UITestRealm::Config config) {
  std::map<std::string, std::string> service_to_component;

  for (const auto& service : ScenicServices(config)) {
    service_to_component[service] = kScenicName;
  }

  for (const auto& service : AccessibilityServices(config)) {
    service_to_component[service] = kA11yManagerName;
  }

  for (const auto& service : SceneOwnerServices(config)) {
    service_to_component[service] = kSceneManagerName;
  }

  // Additional input services specific to the scene_manager scene.
  if (config.use_scene_owner) {
    service_to_component[fuchsia::ui::input::ImeService::Name_] = kTextManagerName;
    service_to_component[fuchsia::ui::input3::Keyboard::Name_] = kTextManagerName;
    service_to_component[fuchsia::input::virtualkeyboard::ControllerCreator::Name_] =
        kVirtualKeyboardManagerName;
    service_to_component[fuchsia::input::virtualkeyboard::Manager::Name_] =
        kVirtualKeyboardManagerName;
  }

  return service_to_component;
}

}  // namespace

UITestRealm::UITestRealm(UITestRealm::Config config) : config_(config) {}

std::string UITestRealm::CalculateBaseRealmUrl() {
  if (config_.use_scene_owner) {
    // Scene manager and input pipeline run in the same monolithic component, so
    // there's no meaningful difference in component topology between the "use
    // input" and "no input" cases.
    return kSceneManagerSceneUrl;
  } else {
    // If no scene owner is present, use the scenic-only realm.
    return kScenicOnlyUrl;
  }
}

void UITestRealm::RouteServices(std::vector<std::string> services, Ref source,
                                std::vector<Ref> targets) {
  if (services.empty()) {
    return;
  }

  std::vector<Capability> protocols;
  for (const auto& service : services) {
    protocols.emplace_back(Protocol{service});
  }

  realm_builder_.AddRoute(
      Route{.capabilities = protocols, .source = std::move(source), .targets = std::move(targets)});
}

component_testing::Realm UITestRealm::AddSubrealm() {
  has_client_subrealm_ = true;
  return realm_builder_.AddChildRealm(kClientSubrealmName);
}

void UITestRealm::ConfigureClientSubrealm() {
  if (!has_client_subrealm_) {
    return;
  }

  // Route default system services to test subrealm.
  RouteServices(DefaultSystemServices(), /* source = */ ParentRef(),
                /* targets = */ {ChildRef{kClientSubrealmName}});

  // Route any passthrough capabilities to the client subrealm.
  if (!config_.passthrough_capabilities.empty()) {
    realm_builder_.AddRoute(Route{.capabilities = config_.passthrough_capabilities,
                                  .source = ParentRef(),
                                  .targets = {ChildRef{kClientSubrealmName}}});
  }

  // Route services to parent that client requested to expose.
  RouteServices(config_.exposed_client_services, /* source = */ ChildRef{kClientSubrealmName},
                /* targets = */ {ParentRef()});

  // Route services client requested from ui subrealm.
  auto service_to_component = GetServiceToComponentMap(config_);
  for (const auto& service : config_.ui_to_client_services) {
    auto it = service_to_component.find(service);
    FX_CHECK(it != service_to_component.end())
        << "Service is not available for the specified realm configuration: " << service;

    RouteServices({service}, /* source = */ ChildRef{it->second},
                  /* targets = */ {ChildRef{kClientSubrealmName}});
  }

  // Route ViewProvider to parent if the client specifies a scene owner.
  if (config_.use_scene_owner) {
    RouteServices({fuchsia::ui::app::ViewProvider::Name_},
                  /* source = */ ChildRef{kClientSubrealmName},
                  /* targets = */ {ParentRef()});
  }

  // TODO(fxbug.dev/98545): Remove this escape hatch, or generalize to any
  // capability.
  //
  // Allow child realm components to access to config-data directory by default.
  //
  // NOTE: The client must offer the "config-data" capability to #realm_builder in
  // its test .cml file.
  realm_builder_.AddRoute(
      {.capabilities = {Directory{
           .name = "config-data", .rights = fuchsia::io::R_STAR_DIR, .path = "/config/data"}},
       .source = ParentRef(),
       .targets = {ChildRef{kClientSubrealmName}}});
}

void UITestRealm::ConfigureAccessibility() {
  // If accessibility_owner is not set, tests do not want test realm include
  // a11y manager.
  if (!config_.accessibility_owner.has_value()) {
    return;
  }

  bool use_real_a11y_manager;
  std::string a11y_manager_url;

  switch (config_.accessibility_owner.value()) {
    case UITestRealm::AccessibilityOwnerType::REAL:
      // We can only enable real a11y manager on flatland because real a11y
      // manager has circular dependency with gfx on
      // `fuchsia.ui.accessibility.view.Registry` from scene_manager to real a11y
      // manager.
      a11y_manager_url = config_.use_flatland ? kRealA11yManagerUrl : kFakeA11yManagerUrl;
      use_real_a11y_manager = true;
      break;
    case AccessibilityOwnerType::FAKE:
      // Fake a11y manager is useful for testing and debugging so test should
      // cover both fake and real a11y manager.
      a11y_manager_url = kFakeA11yManagerUrl;
      use_real_a11y_manager = false;
      break;
  }

  realm_builder_.AddChild(kA11yManagerName, a11y_manager_url);
  RouteServices({fuchsia::logger::LogSink::Name_},
                /* source = */ ParentRef(),
                /* targets = */ {ChildRef{kA11yManagerName}});
  RouteServices({fuchsia::ui::composition::Flatland::Name_, fuchsia::ui::scenic::Scenic::Name_,
                 fuchsia::ui::observation::scope::Registry::Name_,
                 fuchsia::ui::pointer::augment::LocalHit::Name_},
                /* source = */ ChildRef{kScenicName},
                /* targets = */ {ChildRef{kA11yManagerName}});
  RouteServices({fuchsia::accessibility::semantics::SemanticsManager::Name_,
                 test::accessibility::Magnifier::Name_},
                /* source = */ ChildRef{kA11yManagerName},
                /* targets = */ {ParentRef()});

  if (config_.use_scene_owner) {
    if (config_.use_flatland) {
      RouteServices({fuchsia::tracing::provider::Registry::Name_},
                    /* source = */ ParentRef(),
                    /* targets = */ {ChildRef{kA11yManagerName}});
      RouteServices({fuchsia::ui::focus::FocusChainListenerRegistry::Name_},
                    /* source = */ ChildRef{kScenicName},
                    /* targets = */ {ChildRef{kA11yManagerName}});
      RouteServices({fuchsia::accessibility::scene::Provider::Name_,
                     fuchsia::accessibility::ColorTransform::Name_},
                    /* source = */ ChildRef{kA11yManagerName},
                    /* targets = */ {ChildRef{kSceneManagerName}});

    } else {
      RouteServices({fuchsia::accessibility::Magnifier::Name_},
                    /* source = */ ChildRef{kA11yManagerName},
                    /* targets = */ {ChildRef{kSceneManagerName}});
    }
  }

  if (use_real_a11y_manager) {
    realm_builder_.AddChild(kSetUIAccessibility, kSetUIAccessibilityUrl);
    RouteServices({fuchsia::settings::Accessibility::Name_},
                  /* source = */ ChildRef{kSetUIAccessibility},
                  /* targets = */ {ChildRef{kA11yManagerName}});
    realm_builder_.AddChild(kIntl, kIntlUrl);
    RouteServices({fuchsia::intl::PropertyProvider::Name_}, /* source = */ ChildRef{kIntl},
                  /* targets = */ {ChildRef{kA11yManagerName}});
  }
}

void UITestRealm::RouteConfigData() {
  auto config_directory_contents = component_testing::DirectoryContents();
  std::vector<Ref> targets;

  if (config_.use_scene_owner) {
    // Supply a default display rotation.
    config_directory_contents.AddFile("display_rotation", std::to_string(config_.display_rotation));

    FX_CHECK(config_.device_pixel_ratio > 0) << "Device pixel ratio must be positive";
    FX_CHECK(fmodf(static_cast<float>(kDisplayWidthPhysicalPixels), config_.device_pixel_ratio) ==
             0)
        << "DPR must result in integer logical display dimensions";
    FX_CHECK(fmodf(static_cast<float>(kDisplayHeightPhysicalPixels), config_.device_pixel_ratio) ==
             0)
        << "DPR must result in integer logical display dimensions";

    // Pick a display usage + pixel density pair that will result in the
    // desired DPR.
    config_directory_contents.AddFile("display_usage", kDisplayUsageNear);
    auto display_pixel_density = kLowResolutionDisplayPixelDensity * config_.device_pixel_ratio;
    config_directory_contents.AddFile("display_pixel_density",
                                      std::to_string(display_pixel_density));
    targets.push_back(ChildRef{kScenicName});
    targets.push_back(ChildRef{kSceneManagerName});
  }

  if (!targets.empty()) {
    realm_builder_.RouteReadOnlyDirectory("config-data", std::move(targets),
                                          std::move(config_directory_contents));
  }
}

void UITestRealm::ConfigureScenic() {
  // Load default config for Scenic, and override its "i_can_haz_flatland" flag.
  realm_builder_.InitMutableConfigFromPackage(kScenicName);
  realm_builder_.SetConfigValue(kScenicName, "flatland_enable_display_composition",
                                ConfigValue::Bool(false));
  realm_builder_.SetConfigValue(kScenicName, "i_can_haz_flatland",
                                ConfigValue::Bool(config_.use_flatland));
}

void UITestRealm::ConfigureSceneOwner() {
  if (!config_.use_scene_owner) {
    return;
  }

  auto input_config_directory_contents = component_testing::DirectoryContents();
  std::vector<Ref> targets;

  // Configure light sensor in Scene Manager.
  targets.push_back(ChildRef{kSceneManagerName});
  input_config_directory_contents.AddFile("empty.json", "");
  realm_builder_.RouteReadOnlyDirectory("sensor-config", std::move(targets),
                                        std::move(input_config_directory_contents));

  // Configure scene provider, which will only be present in the test realm if
  // the client specifies a scene owner. Note: scene-provider has more config
  // fields than we set here, load the defaults.
  realm_builder_.InitMutableConfigFromPackage(kSceneProviderName);
  realm_builder_.SetConfigValue(kSceneProviderName, "use_flatland",
                                ConfigValue::Bool(config_.use_flatland));
}

void UITestRealm::Build() {
  // Set up a11y manager, if requested, and route semantics manager service to
  // client subrealm.
  //
  // NOTE: We opt to configure accessibility dynamically, rather then in the
  // .cml for the base realms, because there are three different a11y
  // configurations (fake, real, none), which can each apply to scenic-only or
  // scene_manager-controlled scenes. The a11y service routing is also different
  // for gfx and flatland, so it would be unwieldy to create a separate static
  // declaration for every a11y configuration tested.
  ConfigureAccessibility();

  // Override flatland flags in Scenic configuration.
  ConfigureScenic();

  // Route config data directories to appropriate recipients (currently, scenic
  // and scene manager are the only use cases for config files.
  RouteConfigData();

  // Configure Scene Manager if it is in use as the scene owner. This includes:
  // * routing input pipeline config data directories to Scene Manager
  // * overriding component config to specify how long the idle threshold
  //   timeout should be for the
  // activity service
  // * overriding component config for scene provider to specify which API to
  //   use to attach the client view to the scene.
  ConfigureSceneOwner();

  // This step needs to come after ConfigureAccessibility(), because the a11y
  // manager component needs to be added to the realm first.
  ConfigureClientSubrealm();

  realm_builder_.AddRoute(Route{
      .capabilities = {Storage{"tmp"}}, .source = ParentRef(), .targets = {ChildRef{kScenicName}}});

  realm_root_ = realm_builder_.Build();
}

void UITestRealm::Teardown(component_testing::ScopedChild::TeardownCallback on_teardown_complete) {
  realm_root_->Teardown(std::move(on_teardown_complete));
}

std::unique_ptr<sys::ServiceDirectory> UITestRealm::CloneExposedServicesDirectory() {
  FX_CHECK(realm_root_)
      << "Client must call Build() before attempting to take exposed services directory";

  return std::make_unique<sys::ServiceDirectory>(realm_root_->component().CloneExposedDir());
}

}  // namespace ui_testing
