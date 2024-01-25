// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_BIN_I2CUTIL2_H_
#define SRC_DEVICES_I2C_BIN_I2CUTIL2_H_

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>

#include <vector>

#include "args.h"

namespace i2cutil {

// TODO(https://fxbug.dev/42055179): Currently there's a mismatch between the maximum
//                         number of transactions the i2c.fidl and i2c-impl.fidl
//                         permit.
//                         This implementation just picks the smaller (safer) of
//                         the two options. When the two are coalesced with one
//                         another this can be removed.
constexpr size_t kMaxTransactionCount =
    std::min(fuchsia_hardware_i2c::wire::kMaxCountTransactions, I2C_IMPL_MAX_RW_OPS);

zx_status_t execute(fidl::ClientEnd<fuchsia_hardware_i2c::Device> client,
                    std::vector<i2cutil::TransactionData>& transactions);

}  // namespace i2cutil

#endif  // SRC_DEVICES_I2C_BIN_I2CUTIL2_H_
