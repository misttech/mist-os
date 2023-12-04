// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPAT_CPP_METADATA_H_
#define LIB_DRIVER_COMPAT_CPP_METADATA_H_

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/driver/incoming/cpp/namespace.h>

#include <memory>
#include <string_view>

namespace compat {

namespace internal {
template <typename ReturnType, typename Unpersist>
zx::result<ReturnType> GetMetadata(const std::shared_ptr<fdf::Namespace>& incoming, uint32_t type,
                                   std::string_view instance, Unpersist unpersist) {
  zx::result compat = incoming->Connect<fuchsia_driver_compat::Service::Device>(instance);
  if (compat.is_error()) {
    return compat.take_error();
  }

  fidl::WireResult metadata_result = fidl::WireCall(*compat)->GetMetadata();
  if (!metadata_result.ok()) {
    return zx::error(metadata_result.status());
  }
  if (metadata_result->is_error()) {
    return metadata_result->take_error();
  }

  auto& metadata_vector = metadata_result.value()->metadata;
  for (const auto& metadata : metadata_vector) {
    if (metadata.type != type) {
      continue;
    }
    size_t size;
    zx_status_t status = metadata.data.get_prop_content_size(&size);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    return unpersist(metadata.data, size);
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace internal

// Attempts to talk to a parent to acquire metadata with |type| and then decodes it into |FidlType|.
template <typename FidlType, typename = std::enable_if_t<fidl::IsFidlObject<FidlType>::value>>
zx::result<FidlType> GetMetadata(const std::shared_ptr<fdf::Namespace>& incoming, uint32_t type,
                                 std::string_view instance = "default") {
  return internal::GetMetadata<FidlType>(
      incoming, type, instance, [](const zx::vmo& vmo, size_t size) -> zx::result<FidlType> {
        auto buffer = std::make_unique<uint8_t[]>(size);
        zx_status_t status = vmo.read(buffer.get(), 0, size);
        if (status != ZX_OK) {
          return zx::error(status);
        }

        fit::result result = fidl::Unpersist<FidlType>(cpp20::span(buffer.get(), size));
        if (result.is_error()) {
          return zx::error(result.error_value().status());
        }
        return zx::ok(*result);
      });
}

// Attempts to talk to a parent to acquire metadata with |type| and then decodes it into |FidlType|.
// The result is only valid as long as |arena| remains alive.
template <typename FidlType>
zx::result<fidl::ObjectView<FidlType>> GetMetadata(const std::shared_ptr<fdf::Namespace>& incoming,
                                                   fidl::AnyArena& arena, uint32_t type,
                                                   std::string_view instance = "default") {
  static_assert(
      fidl::IsFidlObject<FidlType>::value,
      "GetMetadata with arena only supported for FIDL types. Check FidlType is correct or remove arena parameter.");
  return internal::GetMetadata<fidl::ObjectView<FidlType>>(
      incoming, type, instance,
      [&arena](const zx::vmo& vmo, size_t size) -> zx::result<fidl::ObjectView<FidlType>> {
        auto buffer = arena.AllocateVector<uint8_t>(size);
        zx_status_t status = vmo.read(buffer, 0, size);
        if (status != ZX_OK) {
          return zx::error(status);
        }

        fit::result result = fidl::InplaceUnpersist<FidlType>(cpp20::span(buffer, size));
        if (result.is_error()) {
          return zx::error(result.error_value().status());
        }
        return zx::ok(*result);
      });
}

// Attempts to talk to a parent to acquire metadata with |type| and then casts it into |ReturnType|.
template <typename ReturnType, typename = std::enable_if_t<!fidl::IsFidlObject<ReturnType>::value>>
zx::result<std::unique_ptr<ReturnType>> GetMetadata(const std::shared_ptr<fdf::Namespace>& incoming,
                                                    uint32_t type,
                                                    std::string_view instance = "default") {
  return internal::GetMetadata<std::unique_ptr<ReturnType>>(
      incoming, type, instance,
      [](const zx::vmo& vmo, size_t size) -> zx::result<std::unique_ptr<ReturnType>> {
        if (size != sizeof(ReturnType)) {
          return zx::error(ZX_ERR_INTERNAL);
        }
        auto ret = std::make_unique<ReturnType>();
        zx_status_t status = vmo.read(ret.get(), 0, size);
        if (status != ZX_OK) {
          return zx::error(status);
        }
        return zx::ok(std::move(ret));
      });
}

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_METADATA_H_
