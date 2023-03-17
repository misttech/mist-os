// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>

namespace component {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenServiceRoot(std::string_view path) {
  // NB: This can't be `return Connect<fuchsia_io::Directory>(path);` because some paths may be both
  // services and directories.
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto& [client, server] = endpoints.value();
  return zx::make_result(
      fdio_open(std::string(path).c_str(), static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                server.TakeChannel().release()),
      std::move(client));
}

}  // namespace component
