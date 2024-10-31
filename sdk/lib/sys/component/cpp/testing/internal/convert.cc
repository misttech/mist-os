// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/component/decl/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/internal/convert.h>
#include <lib/sys/component/cpp/testing/internal/errors.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <zircon/availability.h>

namespace component_testing {
namespace internal {

// Convenience macro to check if a std::optional |field| is present
// and if so, populate the |fidl_type| with it. This is only
// used in |ConvertToFidl|.
#define ZX_COMPONENT_ADD_IF_PRESENT(cpp_type, field, fidl_type) \
  if ((cpp_type)->field.has_value()) {                          \
    (fidl_type).set_##field((cpp_type)->field.value());         \
  }

// Same as above but wraps |field| value in a std::string.
#define ZX_COMPONENT_ADD_STR_IF_PRESENT(cpp_type, field, fidl_type)  \
  if ((cpp_type)->field.has_value()) {                               \
    (fidl_type).set_##field(std::string((cpp_type)->field.value())); \
  }

fuchsia::component::test::ChildOptions ConvertToFidl(const ChildOptions& options) {
  fuchsia::component::test::ChildOptions result;
  result.set_startup(options.startup_mode);
  if (!options.environment.empty()) {
    result.set_environment(std::string(options.environment));
  }

  if (!options.config_overrides.empty()) {
    result.mutable_config_overrides()->reserve(options.config_overrides.size());

    for (const auto& config_override : options.config_overrides) {
      ZX_ASSERT(!config_override.IsEmpty());
      fuchsia::component::decl::ConfigOverride override_clone;
      ZX_COMPONENT_ASSERT_STATUS_OK("ConfigValue/Clone", config_override.Clone(&override_clone));

      result.mutable_config_overrides()->push_back(std::move(override_clone));
    }
  }

  return result;
}

namespace {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
std::string NameFromDictionaryRef(const DictionaryRef& ref) {
  size_t delim = ref.path.find('/');
  if (delim == std::string_view::npos || ref.path.substr(0, delim) != "self") {
    ZX_PANIC("DictionaryRef path must be of the form self/<dictionary_name>");
  }
  std::string_view name = ref.path.substr(delim + 1, ref.path.size());
  if (name.find('/') != std::string_view::npos) {
    ZX_PANIC("DictionaryRef path must be of the form self/<dictionary_name>");
  }
  return std::string(name);
}
#endif
}  // namespace

fuchsia::component::decl::Ref ConvertToFidl(Ref ref) {
  if (auto child_ref = cpp17_get_if<ChildRef>(&ref)) {
    fuchsia::component::decl::ChildRef result;
    result.name = std::string(child_ref->name);
    return fuchsia::component::decl::Ref::WithChild(std::move(result));
  }
  if (auto _ = cpp17_get_if<ParentRef>(&ref)) {
    return fuchsia::component::decl::Ref::WithParent(fuchsia::component::decl::ParentRef());
  }
  if (auto collection_ref = cpp17_get_if<CollectionRef>(&ref)) {
    fuchsia::component::decl::CollectionRef result;
    result.name = std::string(collection_ref->name);
    return fuchsia::component::decl::Ref::WithCollection(std::move(result));
  }
  if (auto _ = cpp17_get_if<FrameworkRef>(&ref)) {
    return fuchsia::component::decl::Ref::WithFramework(fuchsia::component::decl::FrameworkRef());
  }
  if (auto _ = cpp17_get_if<VoidRef>(&ref)) {
    return fuchsia::component::decl::Ref::WithVoidType(fuchsia::component::decl::VoidRef());
  }
  if (auto _ = cpp17_get_if<SelfRef>(&ref)) {
    return fuchsia::component::decl::Ref::WithSelf(fuchsia::component::decl::SelfRef());
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (auto dictionary_ref = cpp17_get_if<DictionaryRef>(&ref)) {
    fuchsia::component::decl::CapabilityRef result;
    result.name = NameFromDictionaryRef(*dictionary_ref);
    return fuchsia::component::decl::Ref::WithCapability(std::move(result));
  }
#endif

  ZX_PANIC("ConvertToFidl(Ref) reached unreachable block!");
}

fuchsia::component::test::Capability ConvertToFidl(Capability capability) {
  if (auto protocol = cpp17_get_if<Protocol>(&capability)) {
    fuchsia::component::test::Protocol fidl_capability;

    fidl_capability.set_name(std::string(protocol->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(protocol, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(protocol, path, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(protocol, type, fidl_capability);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    ZX_COMPONENT_ADD_STR_IF_PRESENT(protocol, from_dictionary, fidl_capability);
#endif
    ZX_COMPONENT_ADD_IF_PRESENT(protocol, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithProtocol(std::move(fidl_capability));
  }
  if (auto service = cpp17_get_if<Service>(&capability)) {
    fuchsia::component::test::Service fidl_capability;

    fidl_capability.set_name(std::string(service->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(service, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(service, path, fidl_capability);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    ZX_COMPONENT_ADD_STR_IF_PRESENT(service, from_dictionary, fidl_capability);
#endif
    ZX_COMPONENT_ADD_IF_PRESENT(service, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithService(std::move(fidl_capability));
  }
  if (auto directory = cpp17_get_if<Directory>(&capability)) {
    fuchsia::component::test::Directory fidl_capability;

    fidl_capability.set_name(std::string(directory->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(directory, as, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(directory, type, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(directory, subdir, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(directory, rights, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(directory, path, fidl_capability);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    ZX_COMPONENT_ADD_STR_IF_PRESENT(directory, from_dictionary, fidl_capability);
#endif
    ZX_COMPONENT_ADD_IF_PRESENT(directory, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithDirectory(std::move(fidl_capability));
  }
  if (auto storage = cpp17_get_if<Storage>(&capability)) {
    fuchsia::component::test::Storage fidl_capability;

    fidl_capability.set_name(std::string(storage->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(storage, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(storage, path, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(storage, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithStorage(std::move(fidl_capability));
  }
  if ([[maybe_unused]] auto dictionary = cpp17_get_if<Dictionary>(&capability)) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    fuchsia::component::test::Dictionary fidl_capability;

    fidl_capability.set_name(std::string(dictionary->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(dictionary, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(dictionary, from_dictionary, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(dictionary, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithDictionary(std::move(fidl_capability));
#else
    ZX_PANIC("Dictionary capabilities are not supported in this API level.");
#endif
  }
  if ([[maybe_unused]] auto config = cpp17_get_if<Config>(&capability)) {
#if FUCHSIA_API_LEVEL_AT_LEAST(20)
    fuchsia::component::test::Config fidl_capability;

    fidl_capability.set_name(std::string(config->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(config, as, fidl_capability);
    ZX_COMPONENT_ADD_IF_PRESENT(config, availability, fidl_capability);

    return fuchsia::component::test::Capability::WithConfig(std::move(fidl_capability));
#else
    ZX_PANIC("Config capabilities are not supported in this API level.");
#endif
  }

  if ([[maybe_unused]] auto resolver = cpp17_get_if<Resolver>(&capability)) {
#if FUCHSIA_API_LEVEL_AT_LEAST(24)
    fuchsia::component::test::Resolver fidl_capability;

    fidl_capability.set_name(std::string(resolver->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(resolver, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(resolver, path, fidl_capability);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    ZX_COMPONENT_ADD_STR_IF_PRESENT(resolver, from_dictionary, fidl_capability);
#endif

    return fuchsia::component::test::Capability::WithResolver(std::move(fidl_capability));
#else
    ZX_PANIC("Resolver capabilities are not supported in this API level.");
#endif
  }

  if ([[maybe_unused]] auto runner = cpp17_get_if<Runner>(&capability)) {
#if FUCHSIA_API_LEVEL_AT_LEAST(24)
    fuchsia::component::test::Runner fidl_capability;

    fidl_capability.set_name(std::string(runner->name));
    ZX_COMPONENT_ADD_STR_IF_PRESENT(runner, as, fidl_capability);
    ZX_COMPONENT_ADD_STR_IF_PRESENT(runner, path, fidl_capability);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    ZX_COMPONENT_ADD_STR_IF_PRESENT(runner, from_dictionary, fidl_capability);
#endif

    return fuchsia::component::test::Capability::WithRunner(std::move(fidl_capability));
#else
    ZX_PANIC("Runner capabilities are not supported in this API level.");
#endif
  }

  ZX_PANIC("ConvertToFidl(Capability) reached unreachable block!");
}

}  // namespace internal
}  // namespace component_testing
