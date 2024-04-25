// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.boot/cpp/fidl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/zx/channel.h>
#include <lib/zx/job.h>
#include <stdio.h>

zx::resource GetProfileResource() {
  zx::result local = component::Connect<fuchsia_kernel::ProfileResource>();
  if (!local.is_ok()) {
    fprintf(stderr, "unable to open fuchsia.boot.ProfileResource channel\n");
    return zx::resource();
  }

  auto result = fidl::WireCall(*local)->Get();
  if (!result.ok()) {
    fprintf(stderr, "unable to get profile resource %d\n", result.error().status());
    return zx::resource();
  }

  return std::move(result->resource);
}
