// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/component/incoming/cpp/service_client.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/array.h>

namespace component {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenServiceRoot(std::string_view path) {
  return component::Connect<fuchsia_io::Directory>(path);
}

}  // namespace component

namespace component {

namespace {

constexpr uint64_t kMaxFilename = fuchsia_io::wire::kMaxFilename;

// Max path length will be two path components, separated by a file separator.
constexpr uint64_t kMaxPath = (2 * kMaxFilename) + 1;

zx::result<fidl::StringView> ValidateAndJoinPath(fidl::Array<char, kMaxPath>* buffer,
                                                 fidl::StringView service,
                                                 fidl::StringView instance) {
  if (service.empty() || service.size() > kMaxFilename) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (instance.size() > kMaxFilename) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (service[0] == '/') {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const uint64_t path_size = service.size() + instance.size() + 1;
  ZX_ASSERT(path_size <= kMaxPath);

  char* path_cursor = buffer->data();
  memcpy(path_cursor, service.data(), service.size());
  path_cursor += service.size();
  *path_cursor++ = '/';
  memcpy(path_cursor, instance.data(), instance.size());
  return zx::ok(fidl::StringView::FromExternal(buffer->data(), path_size));
}

}  // namespace

zx::result<> OpenNamedServiceAt(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                                std::string_view service, std::string_view instance,
                                zx::channel remote) {
  fidl::Array<char, kMaxPath> path_buffer;
  zx::result<fidl::StringView> path_result =
      ValidateAndJoinPath(&path_buffer, fidl::StringView::FromExternal(service),
                          fidl::StringView::FromExternal(instance));
  if (!path_result.is_ok()) {
    return path_result.take_error();
  }
  return internal::DirectoryOpenFunc(dir.channel(), path_result.value(),
                                     fidl::internal::MakeAnyTransport(std::move(remote)));
}

}  // namespace component
