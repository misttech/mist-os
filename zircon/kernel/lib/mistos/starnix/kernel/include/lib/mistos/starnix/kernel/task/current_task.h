// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_CURRENT_TASK_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_CURRENT_TASK_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>
#include <lib/mistos/starnix/kernel/loader.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/task/pidtable.h>
#include <lib/mistos/starnix/kernel/vfs/fd_numbers.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <zircon/types.h>

#include <utility>
#include <vector>

#include <fbl/ref_ptr.h>
#include <fbl/string.h>

namespace starnix {

class Task;

// The thread related information of a `CurrentTask`. The information should never be used  outside
// of the thread owning the `CurrentTask`.
struct ThreadState {
  // A copy of the registers associated with the Zircon thread. Up-to-date values can be read
  // from `self.handle.read_state_general_regs()`. To write these values back to the thread, call
  // `self.handle.write_state_general_regs(self.thread_state.registers.into())`.
  RegisterState registers;

  /// Copy of the current extended processor state including floating point and vector registers.
  // pub extended_pstate: ExtendedPstateState,

  /// A custom function to resume a syscall that has been interrupted by SIGSTOP.
  /// To use, call set_syscall_restart_func and return ERESTART_RESTARTBLOCK. sys_restart_syscall
  /// will eventually call it.
  // pub syscall_restart_func: Option<Box<SyscallRestartFunc>>,
};

class TaskBuilder {
 public:
  TaskBuilder(fbl::RefPtr<Task> task) : task_(std::move(task)) {}

  fbl::RefPtr<Task> task() const { return task_; }

  Task* operator->() {
    ASSERT_MSG(task_, "called `operator->` empty Task");
    return task_.get();
  }

 private:
  // The underlying task object.
  fbl::RefPtr<Task> task_;

  // ThreadState thread_state_;
};

class Kernel;

// The task object associated with the currently executing thread.
//
// We often pass the `CurrentTask` as the first argument to functions if those functions need to
// know contextual information about the thread on which they are running. For example, we often
// use the `CurrentTask` to perform access checks, which ensures that the caller is authorized to
// perform the requested operation.
//
// The `CurrentTask` also has state that can be referenced only on the currently executing thread,
// such as the register state for that thread. Syscalls are given a mutable references to the
// `CurrentTask`, which lets them manipulate this state.
//
// See also `Task` for more information about tasks.
class CurrentTask : public MemoryAccessor {
 public:
  static CurrentTask From(const TaskBuilder& builder) { return std::move(builder.task()); }

  // Accessors.
  const fbl::RefPtr<Task>& task() const;
  fbl::RefPtr<Task> task();

  // Creates the initial process for a kernel.
  //
  // The created process will be a task that is the leader of a new thread group.
  //
  // The init process is special because it's the root of the parent/child relationship between
  // tasks. If a task dies, the init process is ultimately responsible for waiting on that task
  // and removing it from the zombie list.
  //
  // It's possible for the kernel to create tasks whose ultimate parent isn't init, but such
  // tasks cannot be created by userspace directly.
  //
  // This function should only be called as part of booting a kernel instance. To create a
  // process after the kernel has already booted, consider `CreateInitChildProcess`
  // or `CreateSystemTask`.
  //
  // The process created by this function should always have pid 1. We require the caller to
  // pass the `pid` as an argument to clarify that it's the callers responsibility to determine
  // the pid for the process.
  static fit::result<Errno, TaskBuilder> create_init_process(const fbl::RefPtr<Kernel>& kernel,
                                                             pid_t pid,
                                                             const fbl::String& initial_name,
                                                             fbl::RefPtr<FsContext> fs);

  // Create a process that is a child of the `init` process.
  //
  // The created process will be a task that is the leader of a new thread group.
  //
  // Most processes are created by userspace and are descendants of the `init` process. In
  // some situations, the kernel needs to create a process itself. This function is the
  // preferred way of creating an actual userspace process because making the process a child of
  // `init` means that `init` is responsible for waiting on the process when it dies and thereby
  // cleaning up its zombie.
  //
  // If you just need a kernel task, and not an entire userspace process, consider using
  // `create_system_task` instead. Even better, consider using the `kthreads` threadpool.
  //
  // This function creates an underlying Zircon process to host the new task.
  static fit::result<Errno, TaskBuilder> create_init_child_process(
      const fbl::RefPtr<Kernel>& kernel, const fbl::String& initial_name);

  fit::result<Errno> exec(const FileHandle& executable, const fbl::String& path,
                          const fbl::Vector<fbl::String>& argv,
                          const fbl::Vector<fbl::String>& environ);

  /// Determine namespace node indicated by the dir_fd.
  ///
  /// Returns the namespace node and the path to use relative to that node.
  fit::result<Errno, std::pair<NamespaceNode, FsStr>> resolve_dir_fd(FdNumber dir_fd, FsStr path,
                                                                     ResolveFlags flags);

  static fit::result<Errno, FileHandle> open_file_bootfs(
      const fbl::String& path /*, OpenFlags flags*/);

  // A convenient wrapper for opening files relative to FdNumber::AT_FDCWD.
  ///
  /// Returns a FileHandle but does not install the FileHandle in the FdTable
  /// for this task.
  fit::result<Errno, FileHandle> open_file(const FsStr& path, OpenFlags flags);

  /// Resolves a path for open.
  ///
  /// If the final path component points to a symlink, the symlink is followed (as long as
  /// the symlink traversal limit has not been reached).
  ///
  /// If the final path component (after following any symlinks, if enabled) does not exist,
  /// and `flags` contains `OpenFlags::CREAT`, a new node is created at the location of the
  /// final path component.
  ///
  /// This returns the resolved node, and a boolean indicating whether the node has been created.
  fit::result<Errno, std::pair<NamespaceNode, bool>> resolve_open_path(
      LookupContext& context, NamespaceNode dir, const FsStr& path, FileMode mode, OpenFlags flags);

  // The primary entry point for opening files relative to a task.
  ///
  /// Absolute paths are resolve relative to the root of the FsContext for
  /// this task. Relative paths are resolve relative to dir_fd. To resolve
  /// relative to the current working directory, pass FdNumber::AT_FDCWD for
  /// dir_fd.
  ///
  /// Returns a FileHandle but does not install the FileHandle in the FdTable
  /// for this task.
  fit::result<Errno, FileHandle> open_file_at(FdNumber dir_fd, const FsStr& path, OpenFlags flags,
                                              FileMode mode, ResolveFlags resolve_flags);

  fit::result<Errno, FileHandle> open_namespace_node_at(NamespaceNode dir, const FsStr& path,
                                                        OpenFlags flags, FileMode mode,
                                                        ResolveFlags& resolve_flags);

  /// A wrapper for FsContext::lookup_parent_at that resolves the given
  /// dir_fd to a NamespaceNode.
  ///
  /// Absolute paths are resolve relative to the root of the FsContext for
  /// this task. Relative paths are resolve relative to dir_fd. To resolve
  /// relative to the current working directory, pass FdNumber::AT_FDCWD for
  /// dir_fd.
  fit::result<Errno, std::pair<NamespaceNode, FsString>> lookup_parent_at() const;

  /// Lookup the parent of a namespace node.
  ///
  /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
  /// calling this function directly.
  ///
  /// This function resolves all but the last component of the given path.
  /// The function returns the parent directory of the last component as well
  /// as the last component.
  ///
  /// If path is empty, this function returns dir and an empty path.
  /// Similarly, if path ends with "." or "..", these components will be
  /// returned along with the parent.
  ///
  /// The returned parent might not be a directory.
  fit::result<Errno, std::pair<NamespaceNode, FsString>> lookup_parent(LookupContext& context,
                                                                       const NamespaceNode& dir,
                                                                       const FsStr& path) const;

  /// Lookup a namespace node.
  ///
  /// Consider using Task::open_file_at or Task::lookup_parent_at rather than
  /// calling this function directly.
  ///
  /// This function resolves the component of the given path.
  fit::result<Errno, NamespaceNode> lookup_path(LookupContext& context, NamespaceNode dir,
                                                const FsStr& path) const;

  /// Lookup a namespace node starting at the root directory.
  ///
  /// Resolves symlinks.
  fit::result<Errno, NamespaceNode> lookup_path_from_root(const FsStr& path) const;

  Task* operator->() const;

  fit::result<Errno, size_t> write_memory(UserAddress addr,
                                          const ktl::span<const ktl::byte>& bytes) const final;

  const ThreadState& thread_state() const { return thread_state_; }

 private:
  CurrentTask(fbl::RefPtr<Task> task);

  template <typename TaskInfoFactory>
  static fit::result<Errno, TaskBuilder> create_task(const fbl::RefPtr<Kernel>& kernel,
                                                     const fbl::String& initial_name,
                                                     fbl::RefPtr<FsContext> root_fs,
                                                     TaskInfoFactory&& task_info_factory);

  template <typename TaskInfoFactory>
  static fit::result<Errno, TaskBuilder> create_task_with_pid(const fbl::RefPtr<Kernel>& kernel,
                                                              PidTable& pids, pid_t pid,
                                                              const fbl::String& initial_name,
                                                              fbl::RefPtr<FsContext> root_fs,
                                                              TaskInfoFactory&& task_info_factory);

  // After the memory is unmapped, any failure in exec is unrecoverable and results in the
  // process crashing. This function is for that second half; any error returned from this
  // function will be considered unrecoverable.
  fit::result<Errno> finish_exec(const fbl::String& path, const ResolvedElf& resolved_elf);

  // The underlying task object.
  fbl::RefPtr<Task> task_;

  ThreadState thread_state_;

  /// Makes CurrentTask neither Sync not Send.
  //_local_marker: PhantomData<*mut u8>,
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_CURRENT_TASK_H_
