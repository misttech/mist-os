// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_INTERNAL_START_ARGS_H_
#define LIB_DRIVER_COMPONENT_CPP_INTERNAL_START_ARGS_H_

#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.data/cpp/fidl.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <vector>

namespace fdf_internal {

// |AdoptEncodedFidlMessage| adopts ownership of the handles in the message
// referenced by an |EncodedFidlMessage| C object. Once constructed, one may
// pass the result from |TakeMessage| into a FIDL decoding function to decode
// into the desired domain object.
//
// On start, drivers may use |AdoptEncodedFidlMessage| to convert an ABI stable
// |EncodedFidlMessage| type into an |EncodedMessage| C++ object expected by the
// FIDL bindings. Example:
//
//     static zx_status_t Start(
//         EncodedDriverStartArgs encoded_start_args, fdf_dispatcher_t* dispatcher, void** driver) {
//       auto wire_format_metadata =
//           fidl::WireFormatMetadata::FromOpaque(encoded_start_args.wire_format_metadata);
//       fdf_internal::AdoptEncodedFidlMessage encoded{encoded_start_args.msg};
//       fit::result start_args = fidl::StandaloneDecode<fuchsia_driver_framework::DriverStartArgs>(
//           encoded.TakeMessage(), wire_format_metadata);
//       // ...
//     }
//
class AdoptEncodedFidlMessage {
 public:
  explicit AdoptEncodedFidlMessage(const EncodedFidlMessage& msg) {
    metadata_.reserve(msg.num_handles);
    fidl_incoming_msg_t c_msg{
        .bytes = msg.bytes,
        .handles = msg.handles,
        .handle_metadata = reinterpret_cast<fidl_handle_metadata_t*>(metadata_.data()),
        .num_bytes = msg.num_bytes,
        .num_handles = msg.num_handles,
    };
    for (size_t i = 0; i < msg.num_handles; i++) {
      // Skip handle type and rights validation since the handles are coming
      // from the driver framework.
      metadata_.emplace_back(fidl_channel_handle_metadata{
          .obj_type = ZX_OBJ_TYPE_NONE,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      });
    }
    message_.emplace(fidl::EncodedMessage::FromEncodedCMessage(c_msg));
  }

  fidl::EncodedMessage TakeMessage() {
    ZX_ASSERT(message_.has_value());
    // NOLINTNEXTLINE
    return *std::exchange(message_, std::nullopt);
  }

 private:
  std::vector<fidl_channel_handle_metadata_t> metadata_;
  std::optional<fidl::EncodedMessage> message_;
};

inline zx::result<std::string> ProgramValue(const fuchsia_data::wire::Dictionary& program,
                                            std::string_view key) {
  if (program.has_entries()) {
    for (auto& entry : program.entries()) {
      if (!std::equal(key.begin(), key.end(), entry.key.begin())) {
        continue;
      }
      if (!entry.value.has_value() || !entry.value->is_str()) {
        return zx::error(ZX_ERR_WRONG_TYPE);
      }
      auto& value = entry.value->str();
      return zx::ok(std::string{value.data(), value.size()});
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

inline zx::result<std::string> ProgramValue(const std::optional<fuchsia_data::Dictionary>& program,
                                            std::string_view key) {
  if (program.has_value() && program->entries().has_value()) {
    for (const auto& entry : *program->entries()) {
      if (key != entry.key()) {
        continue;
      }
      auto value = entry.value()->str();
      if (!value.has_value()) {
        return zx::error(ZX_ERR_WRONG_TYPE);
      }
      return zx::ok(value.value());
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

// Returns the list of values for |key| as a vector of strings.
inline zx::result<std::vector<std::string>> ProgramValueAsVector(
    const fuchsia_data::wire::Dictionary& program, std::string_view key) {
  if (program.has_entries()) {
    for (auto& entry : program.entries()) {
      if (!std::equal(key.begin(), key.end(), entry.key.begin())) {
        continue;
      }
      if (!entry.value.has_value() || !entry.value->is_str_vec()) {
        return zx::error(ZX_ERR_WRONG_TYPE);
      }
      auto& values = entry.value->str_vec();
      std::vector<std::string> result;
      result.reserve(values.count());
      for (auto& value : values) {
        result.emplace_back(std::string{value.data(), value.size()});
      }
      return zx::ok(result);
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

// Returns the list of values for |key| as a vector of strings.
inline zx::result<std::vector<std::string>> ProgramValueAsVector(
    const fuchsia_data::Dictionary& program, std::string_view key) {
  auto program_entries = program.entries();
  if (program_entries.has_value()) {
    for (auto& entry : program_entries.value()) {
      auto& entry_key = entry.key();
      auto& entry_value = entry.value();

      if (key != entry_key) {
        continue;
      }

      if (entry_value->Which() != fuchsia_data::DictionaryValue::Tag::kStrVec) {
        return zx::error(ZX_ERR_WRONG_TYPE);
      }

      return zx::ok(entry_value->str_vec().value());
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

inline zx::result<fidl::UnownedClientEnd<fuchsia_io::Directory>> NsValue(
    const fidl::VectorView<fuchsia_component_runner::wire::ComponentNamespaceEntry>& entries,
    std::string_view path) {
  for (auto& entry : entries) {
    if (std::equal(path.begin(), path.end(), entry.path().begin())) {
      return zx::ok<fidl::UnownedClientEnd<fuchsia_io::Directory>>(entry.directory());
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

inline zx::result<fidl::UnownedClientEnd<fuchsia_io::Directory>> NsValue(
    const std::vector<fuchsia_component_runner::ComponentNamespaceEntry>& entries,
    std::string_view path) {
  for (auto& entry : entries) {
    auto entry_path = entry.path();
    ZX_ASSERT_MSG(entry_path.has_value(), "The entry's path cannot be empty.");
    if (path == entry_path.value()) {
      auto& entry_directory = entry.directory();
      ZX_ASSERT_MSG(entry_directory.has_value(), "The entry's directory cannot be empty.");
      return zx::ok<fidl::UnownedClientEnd<fuchsia_io::Directory>>(entry_directory.value());
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

}  // namespace fdf_internal

#endif  // LIB_DRIVER_COMPONENT_CPP_INTERNAL_START_ARGS_H_
