// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_SCSILIB_CONTROLLER_H_
#define SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_SCSILIB_CONTROLLER_H_

#include <lib/zx/vmo.h>
#include <stdint.h>
#include <sys/uio.h>
#include <zircon/types.h>

#include <optional>

namespace scsi {

struct ScsiLibOp;

class Controller {
 public:
  virtual ~Controller() = default;

  // Size of metadata struct required for each command transaction by this controller. This metadata
  // struct must include scsi::ScsiLibOp as its first (and possibly only) member.
  virtual size_t BlockOpSize() = 0;

  // Synchronously execute a SCSI command on the device at target:lun.
  // |cdb| contains the SCSI CDB to execute.
  // |data| and |is_write| specify optional data-out or data-in regions.
  // Returns ZX_OK if the command was successful at both the transport layer and no check
  // condition happened.
  // Typically used for administrative commands where data resides in process memory.
  virtual zx_status_t ExecuteCommandSync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                         iovec data) = 0;

  // Asynchronously execute a SCSI command on the device at target:lun.
  // |cdb| contains the SCSI CDB to execute.
  // |scsilib_op|, |block_size_bytes|, and |is_write| specify optional data-out or data-in regions.
  // Command execution status is returned by invoking |scsilib_op|->Complete(status).
  // Typically used for IO commands where data may not reside in process memory.
  virtual void ExecuteCommandAsync(uint8_t target, uint16_t lun, iovec cdb, bool is_write,
                                   uint32_t block_size_bytes, ScsiLibOp* scsilib_op) = 0;
};

}  // namespace scsi

#endif  // SRC_DEVICES_BLOCK_LIB_SCSI_INCLUDE_LIB_SCSI_SCSILIB_CONTROLLER_H_
