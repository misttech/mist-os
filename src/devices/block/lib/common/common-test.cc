// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/lib/common/include/common.h"

#include <zxtest/zxtest.h>

namespace block {

class CommonTest : public zxtest::Test {
 public:
  void SetUp() override { fdf::Logger::SetGlobalInstance(&logger_); }

 protected:
  fdf::Logger logger_{"common-test", FUCHSIA_LOG_DEBUG, zx::socket{},
                      fidl::WireClient<fuchsia_logger::LogSink>()};
};

TEST_F(CommonTest, CheckIoRangeTest) {
  block_read_write rw;

  rw = {
      .length = 0,
      .offset_dev = 10,
  };
  EXPECT_EQ(CheckIoRange(rw, 100, logger_), ZX_ERR_OUT_OF_RANGE);

  rw = {
      .length = 11,
      .offset_dev = 90,
  };
  EXPECT_EQ(CheckIoRange(rw, 100, logger_), ZX_ERR_OUT_OF_RANGE);

  rw = {
      .length = 1,
      .offset_dev = 100,
  };
  EXPECT_EQ(CheckIoRange(rw, 100, logger_), ZX_ERR_OUT_OF_RANGE);

  rw = {
      .length = 2,
      .offset_dev = 99,
  };
  EXPECT_EQ(CheckIoRange(rw, 100, logger_), ZX_ERR_OUT_OF_RANGE);

  rw = {
      .length = 101,
      .offset_dev = 0,
  };
  EXPECT_EQ(CheckIoRange(rw, 100, logger_), ZX_ERR_OUT_OF_RANGE);

  rw = {
      .length = 1,
      .offset_dev = 0,
  };
  EXPECT_OK(CheckIoRange(rw, 100, logger_));

  rw = {
      .length = 1,
      .offset_dev = 99,
  };
  EXPECT_OK(CheckIoRange(rw, 100, logger_));

  rw = {
      .length = 100,
      .offset_dev = 0,
  };
  EXPECT_OK(CheckIoRange(rw, 100, logger_));
}

TEST_F(CommonTest, CheckIoRangeMaxTransferTest) {
  block_trim trim;

  trim = {
      .length = 26,
      .offset_dev = 0,
  };
  EXPECT_EQ(CheckIoRange(trim, 100, 25, logger_), ZX_ERR_OUT_OF_RANGE);

  trim = {
      .length = 2,
      .offset_dev = 99,
  };
  EXPECT_EQ(CheckIoRange(trim, 100, 25, logger_), ZX_ERR_OUT_OF_RANGE);

  trim = {
      .length = 25,
      .offset_dev = 0,
  };
  EXPECT_OK(CheckIoRange(trim, 100, 25, logger_));
}

TEST_F(CommonTest, CheckFlushValidTest) {
  block_read_write rw;

  rw = {
      .vmo = 1,
      .length = 0,
      .offset_dev = 0,
      .offset_vmo = 0,
  };
  EXPECT_EQ(CheckFlushValid(rw, logger_), ZX_ERR_INVALID_ARGS);

  rw = {
      .vmo = ZX_HANDLE_INVALID,
      .length = 2,
      .offset_dev = 0,
      .offset_vmo = 0,
  };
  EXPECT_EQ(CheckFlushValid(rw, logger_), ZX_ERR_INVALID_ARGS);

  rw = {
      .vmo = ZX_HANDLE_INVALID,
      .length = 0,
      .offset_dev = 3,
      .offset_vmo = 0,
  };
  EXPECT_EQ(CheckFlushValid(rw, logger_), ZX_ERR_INVALID_ARGS);

  rw = {
      .vmo = ZX_HANDLE_INVALID,
      .length = 0,
      .offset_dev = 0,
      .offset_vmo = 4,
  };
  EXPECT_EQ(CheckFlushValid(rw, logger_), ZX_ERR_INVALID_ARGS);

  rw = {
      .vmo = ZX_HANDLE_INVALID,
      .length = 0,
      .offset_dev = 0,
      .offset_vmo = 0,
  };
  EXPECT_OK(CheckFlushValid(rw, logger_));
}

}  // namespace block
