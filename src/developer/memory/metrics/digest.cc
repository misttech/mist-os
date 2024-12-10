// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/digest.h"

#include <lib/trace/event.h>

#include <ranges>

#include "src/developer/memory/metrics/bucket_match.h"

namespace memory {
Digest::Digest(const Capture& capture, Digester* digester) { digester->Digest(capture, this); }

Digester::Digester(const std::vector<BucketMatch>& bucket_matches)
    : bucket_matches_(bucket_matches) {}

void Digester::Digest(const Capture& capture, class Digest* digest) {
  TRACE_DURATION("memory_metrics", "Digester::Digest");
  digest->time_ = capture.time();
  digest->undigested_vmos_.reserve(capture.koid_to_vmo().size());
  for (const auto& [koid, _] : capture.koid_to_vmo()) {
    digest->undigested_vmos_.emplace(koid);
  }

  digest->buckets_.reserve(bucket_matches_.size());
  for (auto& bucket_match : bucket_matches_) {
    auto& bucket = digest->buckets_.emplace_back(bucket_match.name(), 0);
    for (const auto& [_, process] : capture.koid_to_process()) {
      if (!bucket_match.ProcessMatch(process)) {
        continue;
      }
      for (const auto& v : process.vmos) {
        auto it = digest->undigested_vmos_.find(v);
        if (it == digest->undigested_vmos_.end()) {
          continue;
        }
        const auto& vmo = capture.vmo_for_koid(v);
        if (!bucket_match.VmoMatch(vmo.name)) {
          continue;
        }
        bucket.size_ += vmo.committed_bytes;
        digest->undigested_vmos_.erase(it);
      }
    }
  }

  std::ranges::sort(digest->buckets_,
                    [](const Bucket& a, const Bucket& b) { return a.size() > b.size(); });
  FractionalBytes undigested_size{};
  for (auto v : digest->undigested_vmos_) {
    undigested_size += capture.vmo_for_koid(v).committed_bytes;
  }
  if (undigested_size.integral > 0) {
    digest->buckets_.emplace_back("Undigested", undigested_size.integral);
  }

  const auto& kmem = capture.kmem();
  if (kmem.total_bytes > 0) {
    FractionalBytes vmo_size;
    for (const auto& bucket : digest->buckets_) {
      vmo_size += bucket.size_;
    }
    if (vmo_size.integral < kmem.vmo_bytes) {
      digest->buckets_.emplace_back("Orphaned", kmem.vmo_bytes - vmo_size.integral);
    }

    digest->buckets_.emplace_back("Kernel", kmem.wired_bytes + kmem.total_heap_bytes +
                                                kmem.mmu_overhead_bytes + kmem.ipc_bytes +
                                                kmem.other_bytes);
    digest->buckets_.emplace_back("Free", kmem.free_bytes);

    if (capture.kmem_extended()) {
      const auto& kmem_ext = capture.kmem_extended().value();
      if (kmem_ext.vmo_pager_total_bytes > 0) {
        digest->buckets_.emplace_back("[Addl]PagerTotal", kmem_ext.vmo_pager_total_bytes);
        digest->buckets_.emplace_back("[Addl]PagerNewest", kmem_ext.vmo_pager_newest_bytes);
        digest->buckets_.emplace_back("[Addl]PagerOldest", kmem_ext.vmo_pager_oldest_bytes);
      }
      if (kmem_ext.vmo_discardable_locked_bytes > 0 ||
          kmem_ext.vmo_discardable_unlocked_bytes > 0) {
        digest->buckets_.emplace_back("[Addl]DiscardableLocked",
                                      kmem_ext.vmo_discardable_locked_bytes);
        digest->buckets_.emplace_back("[Addl]DiscardableUnlocked",
                                      kmem_ext.vmo_discardable_unlocked_bytes);
      }
    }

    if (capture.kmem_compression()) {
      digest->buckets_.emplace_back("[Addl]ZramCompressedBytes",
                                    capture.kmem_compression()->compressed_storage_bytes);
    }
  }
}

}  // namespace memory
