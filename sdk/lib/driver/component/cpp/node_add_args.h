// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_NODE_ADD_ARGS_H_
#define LIB_DRIVER_COMPONENT_CPP_NODE_ADD_ARGS_H_

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component.decl/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/traits.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <zircon/availability.h>

#include <string_view>

namespace fdf {

fuchsia_component_decl::Offer MakeOffer(
    std::string_view service_name, std::string_view instance_name = component::kDefaultInstance);

fuchsia_component_decl::wire::Offer MakeOffer(
    fidl::AnyArena& arena, std::string_view service_name,
    std::string_view instance_name = component::kDefaultInstance);

#if FUCHSIA_API_LEVEL_AT_MOST(18)

template <typename Service>
fuchsia_component_decl::Offer MakeOffer(
    std::string_view instance_name = component::kDefaultInstance) {
  static_assert(fidl::IsServiceV<Service>, "Service must be a fidl Service");
  return MakeOffer(Service::Name, instance_name);
}

template <typename Service>
fuchsia_component_decl::wire::Offer MakeOffer(
    fidl::AnyArena& arena, std::string_view instance_name = component::kDefaultInstance) {
  static_assert(fidl::IsServiceV<Service>, "Service must be a fidl Service");
  return MakeOffer(arena, Service::Name, instance_name);
}

#endif  // FUCHSIA_API_LEVEL_AT_MOST(18)

#if FUCHSIA_API_LEVEL_AT_LEAST(18)

template <typename Service>
fuchsia_driver_framework::Offer MakeOffer2(
    std::string_view instance_name = component::kDefaultInstance) {
  static_assert(fidl::IsServiceV<Service>, "Service must be a fidl Service");
  if constexpr (std::is_same_v<typename Service::Transport, fidl::internal::DriverTransport>) {
    return fuchsia_driver_framework::Offer::WithDriverTransport(
        MakeOffer(Service::Name, instance_name));
  } else if constexpr (std::is_same_v<typename Service::Transport,
                                      fidl::internal::ChannelTransport>) {
    return fuchsia_driver_framework::Offer::WithZirconTransport(
        MakeOffer(Service::Name, instance_name));
  } else {
    static_assert(std::false_type{}, "Service must be using DriverTransport or ChannelTransport.");
  }
}

template <typename Service>
fuchsia_driver_framework::wire::Offer MakeOffer2(
    fidl::AnyArena& arena, std::string_view instance_name = component::kDefaultInstance) {
  static_assert(fidl::IsServiceV<Service>, "Service must be a fidl Service");
  if constexpr (std::is_same_v<typename Service::Transport, fidl::internal::DriverTransport>) {
    return fuchsia_driver_framework::wire::Offer::WithDriverTransport(
        arena, MakeOffer(arena, Service::Name, instance_name));
  } else if constexpr (std::is_same_v<typename Service::Transport,
                                      fidl::internal::ChannelTransport>) {
    return fuchsia_driver_framework::wire::Offer::WithZirconTransport(
        arena, MakeOffer(arena, Service::Name, instance_name));
  } else {
    static_assert(std::false_type{}, "Service must be using DriverTransport or ChannelTransport.");
  }
}

#endif  //  FUCHSIA_API_LEVEL_AT_LEAST(18)

inline fuchsia_driver_framework::NodeProperty MakeProperty(std::string_view key,
                                                           std::string_view value) {
  return fuchsia_driver_framework::NodeProperty{
      {.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(std::string(key)),
       .value = fuchsia_driver_framework::NodePropertyValue::WithStringValue(std::string(value))}};
}

inline fuchsia_driver_framework::NodeProperty MakeProperty(std::string_view key,
                                                           const char* value) {
  return MakeProperty(key, std::string_view(value));
}

inline fuchsia_driver_framework::NodeProperty MakeProperty(std::string_view key, bool value) {
  return fuchsia_driver_framework::NodeProperty{
      {.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(std::string(key)),
       .value = fuchsia_driver_framework::NodePropertyValue::WithBoolValue(value)}};
}

inline fuchsia_driver_framework::NodeProperty MakeProperty(std::string_view key, uint32_t value) {
  return fuchsia_driver_framework::NodeProperty{
      {.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(std::string(key)),
       .value = fuchsia_driver_framework::NodePropertyValue::WithIntValue(value)}};
}

inline fuchsia_driver_framework::wire::NodeProperty MakeProperty(fidl::AnyArena& arena,
                                                                 std::string_view key,
                                                                 std::string_view value) {
  return fuchsia_driver_framework::wire::NodeProperty{
      .key = fuchsia_driver_framework::wire::NodePropertyKey::WithStringValue(arena, key),
      .value = fuchsia_driver_framework::wire::NodePropertyValue::WithStringValue(arena, value)};
}

inline fuchsia_driver_framework::wire::NodeProperty MakeProperty(fidl::AnyArena& arena,
                                                                 std::string_view key,
                                                                 const char* value) {
  return MakeProperty(arena, key, std::string_view(value));
}

inline fuchsia_driver_framework::wire::NodeProperty MakeProperty(fidl::AnyArena& arena,
                                                                 std::string_view key, bool value) {
  return fuchsia_driver_framework::wire::NodeProperty{
      .key = fuchsia_driver_framework::wire::NodePropertyKey::WithStringValue(arena, key),
      .value = fuchsia_driver_framework::wire::NodePropertyValue::WithBoolValue(value)};
}

inline fuchsia_driver_framework::wire::NodeProperty MakeProperty(fidl::AnyArena& arena,
                                                                 std::string_view key,
                                                                 uint32_t value) {
  return fuchsia_driver_framework::wire::NodeProperty{
      .key = fuchsia_driver_framework::wire::NodePropertyKey::WithStringValue(arena, key),
      .value = fuchsia_driver_framework::wire::NodePropertyValue::WithIntValue(value)};
}

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_NODE_ADD_ARGS_H_
