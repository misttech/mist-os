// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_START_ARGS_H_
#define LIB_DRIVER_TESTING_CPP_START_ARGS_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/test_node.h>

namespace fdf_testing {

struct CreateStartArgsResult {
  fdf::DriverStartArgs start_args;
  fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server;
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
};

zx::result<CreateStartArgsResult> CreateStartArgs(fdf_testing::TestNode& node_server);

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_START_ARGS_H_
