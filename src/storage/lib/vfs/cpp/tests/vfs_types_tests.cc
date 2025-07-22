// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/common_types.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs {
namespace {

namespace fio = fuchsia_io;
class DummyVnode : public Vnode {
 public:
  DummyVnode() = default;
};

#define EXPECT_RESULT_OK(expr) EXPECT_TRUE((expr).is_ok())
#define EXPECT_RESULT_ERROR(error_val, expr) \
  EXPECT_TRUE((expr).is_error());            \
  EXPECT_EQ(error_val, (expr).status_value())

TEST(DeprecatedOptions, DeprecatedValidateOptionsForDirectory) {
  class TestDirectory : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kDirectory; }
  };

  TestDirectory vnode;
  EXPECT_RESULT_OK(vnode.DeprecatedValidateOptions(
      *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_FILE,
                      vnode.DeprecatedValidateOptions(
                          *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

TEST(DeprecatedOptions, DeprecatedValidateOptionsForService) {
  class TestConnector : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kConnector; }
  };

  TestConnector vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.DeprecatedValidateOptions(
                          *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.DeprecatedValidateOptions(
      *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

TEST(DeprecatedOptions, DeprecatedValidateOptionsForFile) {
  class TestFile : public DummyVnode {
   public:
    fio::NodeProtocolKinds GetProtocols() const final { return fio::NodeProtocolKinds::kFile; }
  };

  TestFile vnode;
  EXPECT_RESULT_ERROR(ZX_ERR_NOT_DIR,
                      vnode.DeprecatedValidateOptions(
                          *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kDirectory)));
  EXPECT_RESULT_OK(vnode.DeprecatedValidateOptions(
      *DeprecatedOptions::FromOpen1Flags(fio::OpenFlags::kNotDirectory)));
}

}  // namespace

}  // namespace fs
