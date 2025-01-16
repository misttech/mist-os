// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/component/decl/cpp/fidl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/fd.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/sys/cpp/component_context.h>

#include <optional>
#include <utility>

namespace driver_test_realm {

using Ref = component_testing::Ref;
using DictionaryRef = component_testing::DictionaryRef;
using SelfRef = component_testing::SelfRef;
using Route = component_testing::Route;
using Protocol = component_testing::Protocol;
using Dictionary = component_testing::Dictionary;
using ParentRef = component_testing::ParentRef;
using ChildRef = component_testing::ChildRef;
using Directory = component_testing::Directory;
using Service = component_testing::Service;
using Capability = component_testing::Capability;

constexpr std::string_view kComponentName = "driver_test_realm";

void Setup(component_testing::RealmBuilder& realm_builder) {
  // Add the driver_test_realm child from the manifest.
  realm_builder.AddChild(std::string(kComponentName), "#meta/driver_test_realm.cm");

  // Offers from parent to driver_test_realm.
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.component.resolution.Resolver-hermetic"}},
      .source = {ParentRef()},
      .targets = {ChildRef{kComponentName}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.pkg.PackageResolver-hermetic"}},
      .source = {ParentRef()},
      .targets = {ChildRef{kComponentName}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.development.Manager"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.test.Realm"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.system.state.Administrator"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Dictionary{.name = "diagnostics"}},
      .source = {ParentRef()},
      .targets = {ChildRef{kComponentName}},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.diagnostics.ArchiveAccessor"}},
      .source = {ParentRef()},
      .targets = {ChildRef{kComponentName}},
  });

  // Exposes from the driver_test_realm to the parent.
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "dev-class", .rights = fuchsia::io::R_STAR_DIR}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Directory{.name = "dev-topological", .rights = fuchsia::io::R_STAR_DIR}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.system.state.Administrator"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.development.Manager"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.framework.CompositeNodeManager"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.registrar.DriverRegistrar"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{"fuchsia.driver.test.Realm"}},
      .source = {ChildRef{kComponentName}},
      .targets = {ParentRef()},
  });
}

void AddDtrExposes(component_testing::RealmBuilder& realm_builder,
                   const std::vector<fuchsia_component_test::Capability>& exposes) {
  auto decl = realm_builder.GetComponentDecl(std::string(kComponentName));
  for (const auto& expose : exposes) {
    const auto& service = expose.service();
    ZX_ASSERT(service.has_value());
    auto name = service->name().value();

    fuchsia::component::decl::ExposeService service_decl;
    service_decl
        .set_source(fuchsia::component::decl::Ref::WithCollection(
            fuchsia::component::decl::CollectionRef{.name = "realm_builder"}))
        .set_source_name(name)
        .set_target_name(name)
        .set_target(
            fuchsia::component::decl::Ref::WithParent(fuchsia::component::decl::ParentRef{}))
        .set_availability(fuchsia::component::decl::Availability::REQUIRED);

    decl.mutable_exposes()->emplace_back(
        fuchsia::component::decl::Expose::WithService(std::move(service_decl)));
  }
  realm_builder.ReplaceComponentDecl(std::string(kComponentName), std::move(decl));

  for (const auto& expose : exposes) {
    realm_builder.AddRoute(Route{
        .capabilities =
            std::vector<Capability>{
                Service{
                    .name = expose.service()->name().value(),
                    .as = expose.service()->as(),
                    .path = expose.service()->path(),
                    .from_dictionary = expose.service()->from_dictionary(),
                },
            },
        .source = {ChildRef{kComponentName}},
        .targets = {ParentRef()},
    });
  }
}

void AddDtrOffers(component_testing::RealmBuilder& realm_builder, Ref from,
                  const std::vector<fuchsia_component_test::Capability>& offers) {
  auto decl = realm_builder.GetComponentDecl(std::string(kComponentName));
  for (const auto& offer : offers) {
    const auto& protocol = offer.protocol();
    ZX_ASSERT(protocol.has_value());
    auto name = protocol->name().value();

    fuchsia::component::decl::OfferProtocol protocol_decl;
    protocol_decl
        .set_source(
            fuchsia::component::decl::Ref::WithParent(fuchsia::component::decl::ParentRef{}))

        .set_source_name(name)
        .set_target_name(name)
        .set_target(fuchsia::component::decl::Ref::WithCollection(
            fuchsia::component::decl::CollectionRef{.name = "realm_builder"}))
        .set_availability(fuchsia::component::decl::Availability::REQUIRED)
        .set_dependency_type(fuchsia::component::decl::DependencyType::STRONG);

    decl.mutable_offers()->emplace_back(
        fuchsia::component::decl::Offer::WithProtocol(std::move(protocol_decl)));
  }
  realm_builder.ReplaceComponentDecl(std::string(kComponentName), std::move(decl));

  for (const auto& offer : offers) {
    std::optional<fuchsia::component::decl::DependencyType> type = std::nullopt;
    if (offer.protocol()->type().has_value()) {
      type.emplace(
          static_cast<fuchsia::component::decl::DependencyType>(offer.protocol()->type().value()));
    }
    realm_builder.AddRoute(Route{
        .capabilities =
            std::vector<Capability>{
                Protocol{
                    .name = offer.protocol()->name().value(),
                    .as = offer.protocol()->as(),
                    .type = type,
                    .path = offer.protocol()->path(),
                    .from_dictionary = offer.protocol()->from_dictionary(),
                },
            },
        .source = {from},
        .targets = {ChildRef{kComponentName}},
    });
  }
}

}  // namespace driver_test_realm
