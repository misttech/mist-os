// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <zircon/errors.h>

#include <type_traits>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace {

namespace fio = fuchsia_io;
class DummyVnode : public fs::Vnode {
 public:
  DummyVnode() = default;
};

#define EXPECT_RESULT_OK(expr) EXPECT_TRUE((expr).is_ok())
#define EXPECT_RESULT_ERROR(error_val, expr) \
  EXPECT_TRUE((expr).is_error());            \
  EXPECT_EQ(error_val, (expr).status_value())

TEST(VnodeConnectionOptions, ValidateOptionsForDirectory) {
  class TestDirectory : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kDirectory;
    }
  };

  TestDirectory vnode;
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_FILE,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForService) {
  class TestConnector : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kConnector;
    }
  };

  TestConnector vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kNotDirectory)));
}

TEST(VnodeConnectionOptions, ValidateOptionsForFile) {
  class TestFile : public DummyVnode {
   public:
    fuchsia_io::NodeProtocolKinds GetProtocols() const final {
      return fuchsia_io::NodeProtocolKinds::kFile;
    }
  };

  TestFile vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.ValidateOptions(fs::VnodeConnectionOptions::FromIoV1Flags(
                          fio::wire::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.ValidateOptions(
      fs::VnodeConnectionOptions::FromIoV1Flags(fio::wire::OpenFlags::kNotDirectory)));
}

}  // namespace
