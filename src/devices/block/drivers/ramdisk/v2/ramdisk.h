// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_H_
#define SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_H_

#include <fidl/fuchsia.hardware.ramdisk/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <memory>
#include <mutex>
#include <vector>

#include "src/devices/block/drivers/ramdisk/v2/ramdisk-controller.h"
#include "src/storage/lib/block_server/block_server.h"

namespace ramdisk_v2 {

class Ramdisk : public fidl::WireServer<fuchsia_hardware_ramdisk::Ramdisk>,
                public block_server::Interface {
 public:
  static zx::result<std::unique_ptr<Ramdisk>> Create(
      RamdiskController* controller, async_dispatcher_t* dispatcher, zx::vmo vmo,
      const block_server::PartitionInfo& partition_info, component::OutgoingDirectory outgoing,
      int id, bool publish);

  Ramdisk(const Ramdisk&) = delete;
  Ramdisk& operator=(const Ramdisk&) = delete;
  ~Ramdisk();

  // FIDL interface Ramdisk
  void SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) override;
  void Wake(WakeCompleter::Sync& completer) override;
  void SleepAfter(SleepAfterRequestView request, SleepAfterCompleter::Sync& completer) override;
  void GetBlockCounts(GetBlockCountsCompleter::Sync& completer) override;

 private:
  Ramdisk(RamdiskController* controller, fzl::OwnedVmoMapper mapping,
          const block_server::PartitionInfo& partition_info, component::OutgoingDirectory outgoing)
      : controller_(controller),
        block_size_(partition_info.block_size),
        block_count_(partition_info.block_count),
        mapping_(std::move(mapping)),
        outgoing_(std::move(outgoing)),
        block_server_(partition_info, this) {}

  void StartThread(block_server::Thread thread) override;
  void OnNewSession(block_server::Session) override;
  void OnRequests(const block_server::Session& session,
                  cpp20::span<block_server::Request>) override;

  // Reads or writes `block_count` blocks (it ignores the count in `request`) at
  // `request_block_offset` blocks relative to the request.
  zx_status_t ReadWrite(const block_server::Request& request, uint64_t request_block_offset,
                        uint64_t block_count) TA_EXCL(lock_);

  // Returns true if we should fail requests (due to being asleep).
  bool ShouldFailRequests() const TA_REQ(lock_);

  RamdiskController* controller_;
  const uint32_t block_size_;
  const uint64_t block_count_;

  fzl::OwnedVmoMapper mapping_;

  // Guards fields of the ramdisk which may be accessed concurrently.
  std::mutex lock_;
  std::condition_variable condition_;

  fuchsia_hardware_ramdisk::wire::BlockWriteCounts block_counts_ TA_GUARDED(lock_);
  std::optional<uint64_t> pre_sleep_write_block_count_ TA_GUARDED(lock_);
  std::vector<uint64_t> blocks_written_since_last_flush_ TA_GUARDED(lock_);

  // Flags modified by SetFlags.
  fuchsia_hardware_ramdisk::RamdiskFlag flags_ TA_GUARDED(lock_);

  component::OutgoingDirectory outgoing_;
  block_server::BlockServer block_server_;
};

}  // namespace ramdisk_v2

#endif  // SRC_DEVICES_BLOCK_DRIVERS_RAMDISK_V2_RAMDISK_H_
