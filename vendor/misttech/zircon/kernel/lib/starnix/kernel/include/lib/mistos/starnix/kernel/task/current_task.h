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
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>
#include <lib/user_copy/user_ptr.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>

namespace starnix {

namespace testing {
class AutoReleasableTask;
}

class Task;
class FsContext;

// The thread related information of a `CurrentTask`. The information should never be used outside
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

 public:
  /// impl ThreadState

  /// Returns a new `ThreadState` with the same `registers` as this one.
  ThreadState snapshot() const { return ThreadState{this->registers}; }
};

class TaskBuilder {
 public:
  // The underlying task object.
  fbl::RefPtr<Task> task;

  ThreadState thread_state;

  /// impl TaskBuilder
  explicit TaskBuilder(fbl::RefPtr<Task> task);

  Task* operator->();

  // C++
  ~TaskBuilder();
};

class Kernel;
struct LookupContext;
struct NamespaceNode;

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
class CurrentTask : public TaskMemoryAccessor {
 public:
  /// impl From<TaskBuilder> for CurrentTask
  static CurrentTask From(const TaskBuilder& builder);

  /// The underlying task object.
  fbl::RefPtr<Task> task;

  ThreadState thread_state;

  /// impl CurrentTask
  util::WeakPtr<Task> weak_task() const;

  void set_creds(Credentials creds) const;

  /// Determine namespace node indicated by the dir_fd.
  ///
  /// Returns the namespace node and the path to use relative to that node.
  fit::result<Errno, ktl::pair<NamespaceNode, FsStr>> resolve_dir_fd(FdNumber dir_fd, FsStr path,
                                                                     ResolveFlags flags) const;

  // A convenient wrapper for opening files relative to FdNumber::AT_FDCWD.
  ///
  /// Returns a FileHandle but does not install the FileHandle in the FdTable
  /// for this task.
  fit::result<Errno, FileHandle> open_file(const FsStr& path, OpenFlags flags) const;

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
  fit::result<Errno, ktl::pair<NamespaceNode, bool>> resolve_open_path(LookupContext& context,
                                                                       NamespaceNode dir,
                                                                       const FsStr& path,
                                                                       FileMode mode,
                                                                       OpenFlags flags) const;

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
                                              FileMode mode, ResolveFlags resolve_flags) const;

  fit::result<Errno, FileHandle> open_namespace_node_at(NamespaceNode dir, const FsStr& path,
                                                        OpenFlags flags, FileMode mode,
                                                        ResolveFlags& resolve_flags) const;

  /// A wrapper for FsContext::lookup_parent_at that resolves the given
  /// dir_fd to a NamespaceNode.
  ///
  /// Absolute paths are resolve relative to the root of the FsContext for
  /// this task. Relative paths are resolve relative to dir_fd. To resolve
  /// relative to the current working directory, pass FdNumber::AT_FDCWD for
  /// dir_fd.
  fit::result<Errno, ktl::pair<NamespaceNode, FsString>> lookup_parent_at(LookupContext& context,
                                                                          FdNumber dir_fd,
                                                                          const FsStr& path) const;

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
  fit::result<Errno, ktl::pair<NamespaceNode, FsString>> lookup_parent(LookupContext& context,
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

  fit::result<Errno> exec(const FileHandle& executable, const ktl::string_view& path,
                          const fbl::Vector<ktl::string_view>& argv,
                          const fbl::Vector<ktl::string_view>& environ);

 private:
  // After the memory is unmapped, any failure in exec is unrecoverable and results in the
  // process crashing. This function is for that second half; any error returned from this
  // function will be considered unrecoverable.
  fit::result<Errno> finish_exec(const ktl::string_view& path, const ResolvedElf& resolved_elf);

 public:
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
      const fbl::RefPtr<Kernel>& kernel, const ktl::string_view& initial_name);

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
                                                             const ktl::string_view& initial_name,
                                                             fbl::RefPtr<FsContext> fs);

  /// Create a task that runs inside the kernel.
  ///
  /// There is no underlying Zircon process to host the task. Instead, the work done by this task
  /// is performed by a thread in the original Starnix process, possible as part of a thread
  /// pool.
  ///
  /// This function is the preferred way to create a context for doing background work inside the
  /// kernel.
  ///
  /// Rather than calling this function directly, consider using `kthreads`, which provides both
  /// a system task and a threadpool on which the task can do work.
  static fit::result<Errno, CurrentTask> create_system_task(const fbl::RefPtr<Kernel>& kernel,
                                                            fbl::RefPtr<FsContext> fs);

 private:
  template <typename TaskInfoFactory>
  static fit::result<Errno, TaskBuilder> create_task(const fbl::RefPtr<Kernel>& kernel,
                                                     const ktl::string_view& initial_name,
                                                     fbl::RefPtr<FsContext> root_fs,
                                                     TaskInfoFactory&& task_info_factory);

  template <typename TaskInfoFactory>
  static fit::result<Errno, TaskBuilder> create_task_with_pid(
      const fbl::RefPtr<Kernel>& kernel, starnix_sync::RwLock<PidTable>::RwLockWriteGuard& pids,
      pid_t pid, const ktl::string_view& initial_name, fbl::RefPtr<FsContext> root_fs,
      TaskInfoFactory&& task_info_factory);

 public:
  /// Clone this task.
  ///
  /// Creates a new task object that shares some state with this task
  /// according to the given flags.
  ///
  /// Used by the clone() syscall to create both processes and threads.
  ///
  /// The exit signal is broken out from the flags parameter like clone3() rather than being
  /// bitwise-ORed like clone().
  fit::result<Errno, TaskBuilder> clone_task(uint64_t flags,
                                             ktl::optional<Signal> child_exit_signal,
                                             UserRef<pid_t> user_parent_tid,
                                             UserRef<pid_t> user_child_tid) const;

  /// The flags indicates only the flags as in clone3(), and does not use the low 8 bits for the
  /// exit signal as in clone().
  starnix::testing::AutoReleasableTask clone_task_for_test(uint64_t flags,
                                                           ktl::optional<Signal> exit_signal);

  /// impl MemoryAccessor for CurrentTask
  fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                     ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial(UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, size_t> write_memory(UserAddress addr,
                                          const ktl::span<const uint8_t>& bytes) const final;

  fit::result<Errno, size_t> write_memory_partial(
      UserAddress addr, const ktl::span<const uint8_t>& bytes) const final;

  fit::result<Errno, size_t> zero(UserAddress addr, size_t length) const final;

  // impl TaskMemoryAccessor
  UserAddress maximum_valid_address() const final;

  // C++
  ~CurrentTask() override;

  Task* operator->() const;

 private:
  explicit CurrentTask(fbl::RefPtr<Task> task);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_CURRENT_TASK_H_
