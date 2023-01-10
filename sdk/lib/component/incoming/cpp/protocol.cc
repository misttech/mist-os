// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/component/incoming/cpp/protocol.h>

namespace component {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenServiceRoot(std::string_view path) {
  return component::Connect<fuchsia_io::Directory>(path);
}

}  // namespace component
