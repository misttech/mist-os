// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async/dispatcher.h>
#include <lib/sync/completion.h>
#include <threads.h>

#include <fbl/mutex.h>
#include <fbl/ref_counted.h>

#include "src/lib/listnode/listnode.h"

namespace i2c {

class I2cBus : public fbl::RefCounted<I2cBus> {
 public:
  struct TransactOp {
    const uint8_t* data_buffer;
    size_t data_size;
    bool is_read;
    bool stop;
  };

  using TransactCallback = void (*)(void* ctx, zx_status_t status, const TransactOp* op_list,
                                    size_t op_count);

  static zx_status_t CreateAndAddChildren(
      zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, uint32_t bus_id,
      const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels,
      async_dispatcher_t* dispatcher);

  explicit I2cBus(zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, uint32_t bus_id,
                  uint64_t max_transfer_size);
  virtual ~I2cBus() {
    AsyncStop();
    WaitForStop();
  }
  zx_status_t Start(zx_device_t* parent);
  void AsyncStop();
  void WaitForStop();
  virtual void Transact(uint16_t address, const TransactOp* op_list, size_t op_count,
                        TransactCallback callback, void* cookie);

 private:
  // struct representing an I2C transaction.
  struct I2cTxn {
    list_node_t node;
    uint16_t address;
    TransactCallback transact_cb;
    void* cookie;
    size_t length;
    size_t op_count;
    uint64_t trace_id;
  };

  int I2cThread();

  zx_device_t* parent_;
  const ddk::I2cImplProtocolClient i2c_;
  const uint32_t bus_id_;
  const uint64_t max_transfer_;

  list_node_t queued_txns_ __TA_GUARDED(mutex_);
  list_node_t free_txns_ __TA_GUARDED(mutex_);
  sync_completion_t txn_signal_;

  bool shutdown_ __TA_GUARDED(mutex_) = false;
  thrd_t thread_;
  fbl::Mutex mutex_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_
