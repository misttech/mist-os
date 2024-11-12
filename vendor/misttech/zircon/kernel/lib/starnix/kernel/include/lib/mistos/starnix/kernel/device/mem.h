// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_MEM_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_MEM_H_

#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>

namespace starnix {

FileHandle new_null_file(const CurrentTask& current_task, OpenFlags flags);

// A device that discards all writes and returns EOF on reads
class DevNull : public FileOps {
 public:
  // impl FileOps for DevNull
  fileops_impl_seekless();
  fileops_impl_noop_sync();

  fit::result<Errno, size_t> write(const FileObject& file, const CurrentTask& current_task,
                                   size_t offset, InputBuffer* data) const final {
    // Writes to /dev/null on Linux treat the input buffer in an unconventional way. The actual
    // data is not touched and if the input parameters are plausible the device claims to
    // successfully write up to MAX_RW_COUNT bytes.  If the input parameters are outside of the
    // user accessible address space, writes will return EFAULT.

    // For debugging log up to 4096 bytes from the input buffer. We don't care about errors when
    // trying to read data to log. The amount of data logged is chosen arbitrarily.

    size_t bytes_to_log = ktl::min(static_cast<size_t>(4096), data->available());
    size_t bytes_logged = 0;

    if (InputBufferExt* data_ext = static_cast<InputBufferExt*>(data)) {
      auto log_buffer = data_ext->read_to_vec_limited(bytes_to_log);

      if (log_buffer.is_ok()) {
        auto& bytes = log_buffer.value();
        // if (STARNIX_LOG_DEV_NULL_WRITES_AT_INFO) {
        //  log_info("write to devnull: %.*s", static_cast<int>(bytes.size()), bytes.data());
        //}
        bytes_logged = bytes.size();
      }
    }

    return fit::ok(bytes_logged + data->drain());
  }

  fit::result<Errno, size_t> read(const FileObject& file, const CurrentTask& current_task,
                                  size_t offset, OutputBuffer* data) const final {
    return fit::ok(static_cast<size_t>(0));
  }

  // C++
  DevNull() = default;
};

// A device that returns zeros on reads and discards all writes
class DevZero : public FileOps {
 public:
  // impl FileOps for DevZero
  fileops_impl_seekless();
  fileops_impl_noop_sync();

  fit::result<Errno, UserAddress> mmap(const FileObject& file, const CurrentTask& current_task,
                                       DesiredAddress addr, uint64_t memory_offset, size_t length,
                                       ProtectionFlags prot_flags, MappingOptionsFlags options,
                                       NamespaceNode filename) const final {
    // All /dev/zero mappings behave as anonymous mappings.
    //
    // This means that we always create a new zero-filled VMO for this mmap request.
    // Memory is never shared between two mappings of /dev/zero, even if
    // `MappingOptions::SHARED` is set.
    //
    // Similar to anonymous mappings, if this process were to request a shared mapping
    // of /dev/zero and then fork, the child and the parent process would share the
    // VMO created here.
    auto memory = create_anonymous_mapping_memory(length) _EP(memory);

    options |= MappingOptions::ANONYMOUS;

    return current_task->mm()->map_memory(
        addr, memory.value(), memory_offset, length, prot_flags, options,
        // We set the filename here, even though we are creating
        // what is functionally equivalent to an anonymous mapping.
        // Doing so affects the output of `/proc/self/maps` and
        // identifies this mapping as file-based.
        {.type = MappingNameType::File} /*, FileWriteGuardRef(nullptr)*/);
  }

  fit::result<Errno, size_t> write(const FileObject& file, const CurrentTask& current_task,
                                   size_t offset, InputBuffer* data) const final {
    return fit::ok(data->drain());
  }

  fit::result<Errno, size_t> read(const FileObject& file, const CurrentTask& current_task,
                                  size_t offset, OutputBuffer* data) const final {
    return data->zero();
  }

  // C++
  DevZero() = default;
};

void mem_device_init(const CurrentTask& system_task);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_MEM_H_
