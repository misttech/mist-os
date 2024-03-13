// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_BUFFER_WRITER_H_
#define ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_BUFFER_WRITER_H_

#include <cstdint>

#include <vm/pinned_vm_object.h>
#include <vm/vm_address_region.h>

// These types are internal implementation details to the thread sampler, but we define them in a
// header so that we can include them in the tests and override/stub out various parts.
namespace sampler::internal {

// Fwd decl to allow friendship with tests.
namespace thread_sampler_tests {
class TestThreadSamplerState;
}

class AllocatedRecord {
 public:
  AllocatedRecord(uint64_t* start, fxt::WordSize record_size, fbl::RefPtr<VmMapping> mapping)
      : buffer_(start, record_size.SizeInWords()), buffer_mapping_(ktl::move(mapping)) {}
  AllocatedRecord(AllocatedRecord&&) = default;
  AllocatedRecord& operator=(AllocatedRecord&&) = default;
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AllocatedRecord);

  // Implement an FXT serializer record
  void WriteWord(uint64_t word);
  void WriteBytes(const void* buffer, size_t num_bytes);
  void Commit();

 private:
  friend class thread_sampler_tests::TestThreadSamplerState;

  cpp20::span<uint64_t> buffer_;

  // Keep the mapping we are writing to alive
  fbl::RefPtr<VmMapping> buffer_mapping_;
};

class BufferWriter {
 public:
  constexpr BufferWriter() = default;
  BufferWriter(vaddr_t ptr, vaddr_t end, fbl::RefPtr<VmMapping> mapping,
               PinnedVmObject pinned_buffer)
      : ptr_(ptr),
        end_(end),
        buffer_mapping_(std::move(mapping)),
        pinned_buffer_(std::move(pinned_buffer)) {}

  BufferWriter(BufferWriter&&) = default;
  BufferWriter& operator=(BufferWriter&&) = default;
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BufferWriter);
  ~BufferWriter() = default;

  // Implement an FXT serializer that writes to the pinned+mapped buffer
  zx::result<AllocatedRecord> Reserve(uint64_t header);

 private:
  vaddr_t ptr_ = 0;
  vaddr_t end_ = 0;
  fbl::RefPtr<VmMapping> buffer_mapping_;
  PinnedVmObject pinned_buffer_;
};
}  // namespace sampler::internal
#endif  // ZIRCON_KERNEL_LIB_THREAD_SAMPLER_INCLUDE_LIB_THREAD_SAMPLER_BUFFER_WRITER_H_
