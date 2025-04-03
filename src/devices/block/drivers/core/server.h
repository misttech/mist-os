// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_CORE_SERVER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_CORE_SERVER_H_

#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/fzl/fifo.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/types.h>

#include <atomic>
#include <new>
#include <utility>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/mutex.h>
#include <fbl/ref_ptr.h>

#include "block-fifo.h"
#include "iobuffer.h"
#include "message-group.h"
#include "message.h"

// Remaps the dev_offset of block requests based on an internal map.
// TODO(https://fxbug.dev/402515764): For now, this just supports a single entry in the map, which
// is all that is required for GPT partitions.  If we want to support this for FVM, we will need
// to support multiple entries, which requires changing the block server to support request
// splitting.
class OffsetMap {
 public:
  static zx::result<std::unique_ptr<OffsetMap>> Create(
      fuchsia_hardware_block::wire::BlockOffsetMapping initial_mapping);

  // Adjusts `request` by applying the map to dev_offset.
  // Returns false if the request would exceed the range known to OffsetMap.
  bool AdjustRequest(block_fifo_request_t& request) const;

 private:
  explicit OffsetMap(fuchsia_hardware_block::wire::BlockOffsetMapping mapping);

  fuchsia_hardware_block::wire::BlockOffsetMapping mapping_;
};

class Server : public fidl::WireServer<fuchsia_hardware_block::Session> {
 public:
  // Creates a new Server.
  static zx::result<std::unique_ptr<Server>> Create(
      ddk::BlockProtocolClient* bp,
      fidl::ClientEnd<fuchsia_hardware_block::OffsetMap> offset_map = {},
      std::span<const fuchsia_hardware_block::wire::BlockOffsetMapping> initial_mappings = {});

  // This will block until all outstanding messages have been processed.
  ~Server() override;

  // Starts the Server using the current thread
  zx_status_t Serve() TA_EXCL(server_lock_);

  zx::result<zx::fifo> GetFifo();
  zx::result<vmoid_t> AttachVmo(zx::vmo vmo) TA_EXCL(server_lock_) TA_EXCL(server_lock_);
  void Close();

  void GetFifo(GetFifoCompleter::Sync& completer) override;
  void AttachVmo(AttachVmoRequestView request, AttachVmoCompleter::Sync& completer) override;
  void Close(CloseCompleter::Sync& completer) override;

  // Updates the total number of pending requests.
  void TxnEnd();

  // Wrapper around "SendResponse", as a convenience
  // for finishing both one-shot and group-based transactions.
  void FinishTransaction(zx_status_t status, reqid_t reqid, groupid_t group);

  // Send the given response to the client.
  void SendResponse(const block_fifo_response_t& response);

  // Initiates a shutdown of the server.  When this finishes, the server might still be running, but
  // it should terminate shortly.
  void Shutdown();

  // Returns true if the server is about to terminate.
  bool WillTerminate() const;

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(Server);
  Server(ddk::BlockProtocolClient* bp, block_info_t block_info, size_t block_op_size,
         std::unique_ptr<OffsetMap> offset_map);

  // Helper for processing a single message read from the FIFO.
  void ProcessRequest(block_fifo_request_t* request);
  zx_status_t ProcessReadWriteRequest(block_fifo_request_t* request);
  zx_status_t ProcessCloseVmoRequest(block_fifo_request_t* request);
  zx_status_t ProcessFlushRequest(block_fifo_request_t* request);
  zx_status_t ProcessTrimRequest(block_fifo_request_t* request);

  zx_status_t IssueFlushCommand(block_fifo_request_t* request, MessageCompleter completer,
                                bool internal_cmd);

  zx_status_t Read(block_fifo_request_t* requests, size_t* count);

  zx::result<vmoid_t> FindVmoIdLocked() TA_REQ(server_lock_);

  // Sends the request embedded in the message down to the lower layers.
  void Enqueue(std::unique_ptr<Message> message) TA_EXCL(server_lock_);

  fzl::fifo<block_fifo_response_t, block_fifo_request_t> fifo_;
  fzl::fifo<block_fifo_request_t, block_fifo_response_t> fifo_peer_;
  block_info_t info_;
  std::unique_ptr<OffsetMap> offset_map_;
  ddk::BlockProtocolClient* bp_;
  size_t block_op_size_;

  // Used to wait for pending_count_ to drop to zero at shutdown time.
  fbl::ConditionVariable condition_;

  // The number of outstanding requests that have been sent down the stack.
  size_t pending_count_ TA_GUARDED(server_lock_);

  std::unique_ptr<MessageGroup> groups_[MAX_TXN_GROUP_COUNT];

  fbl::Mutex server_lock_;
  fbl::WAVLTree<vmoid_t, fbl::RefPtr<IoBuffer>> tree_ TA_GUARDED(server_lock_);
  vmoid_t last_id_ TA_GUARDED(server_lock_);
};

#endif  // SRC_DEVICES_BLOCK_DRIVERS_CORE_SERVER_H_
