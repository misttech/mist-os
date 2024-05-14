// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/current_task.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/loader.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/fd_numbers.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/userloader/start.h>
#include <lib/mistos/userloader/userloader.h>
#include <lib/mistos/util/strings/split_string.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zbi_parser/zbi.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <lockdep/guard.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace util;

namespace starnix {

CurrentTask::CurrentTask(fbl::RefPtr<Task> task) : task_(std::move(task)) {}

const fbl::RefPtr<Task>& CurrentTask::task() const { return task_; }
fbl::RefPtr<Task> CurrentTask::task() { return task_; }

Task* CurrentTask::operator->() const {
  ASSERT_MSG(task_, "called `operator->()` empty Task");
  return task_.get();
}

fit::result<Errno, TaskBuilder> CurrentTask::create_init_process(const fbl::RefPtr<Kernel>& kernel,
                                                                 pid_t pid,
                                                                 const fbl::String& initial_name,
                                                                 fbl::RefPtr<FsContext> fs) {
  LTRACE;
  Guard<Mutex> guard{kernel->pidtable_rw_lock()};

  auto task_info_factory = [kernel, initial_name](pid_t pid,
                                                  fbl::RefPtr<ProcessGroup> process_group) {
    return create_zircon_process(kernel, {}, pid, process_group, initial_name);
  };

  return create_task_with_pid(kernel, kernel->pids(), pid, initial_name, fs, task_info_factory);
}

fit::result<Errno, TaskBuilder> CurrentTask::create_init_child_process(
    const fbl::RefPtr<Kernel>& kernel, const fbl::String& initial_name) {
  LTRACE;
  fbl::RefPtr<Task> init_task;
  {
    fbl::RefPtr<Task> weak_init;
    {
      Guard<Mutex> lock(kernel->pidtable_rw_lock());
      weak_init = kernel->pids().get_task(1);
    }
    if (!weak_init) {
      return fit::error(errno(EINVAL));
    }
    init_task = weak_init;
  }

  auto task_info_factory = [kernel, initial_name](pid_t pid,
                                                  fbl::RefPtr<ProcessGroup> process_group) {
    return create_zircon_process(kernel, {}, pid, process_group, initial_name);
  };

  return create_task(kernel, initial_name, init_task->fs()->fork(), task_info_factory);
}

template <typename TaskInfoFactory>
fit::result<Errno, TaskBuilder> CurrentTask::create_task(const fbl::RefPtr<Kernel>& kernel,
                                                         const fbl::String& initial_name,
                                                         fbl::RefPtr<FsContext> root_fs,
                                                         TaskInfoFactory&& task_info_factory) {
  LTRACE;
  pid_t pid;
  {
    Guard<Mutex> guard{kernel->pidtable_rw_lock()};
    pid = kernel->pids().allocate_pid();
  }
  return create_task_with_pid(kernel, kernel->pids(), pid, initial_name, root_fs,
                              task_info_factory);
}

template <typename TaskInfoFactory>
fit::result<Errno, TaskBuilder> CurrentTask::create_task_with_pid(
    const fbl::RefPtr<Kernel>& kernel, PidTable& pids, pid_t pid, const fbl::String& initial_name,
    fbl::RefPtr<FsContext> root_fs, TaskInfoFactory&& task_info_factory) {
  LTRACE;
  DEBUG_ASSERT(pids.get_task(pid) == nullptr);

  fbl::RefPtr<ProcessGroup> process_group;
  zx_status_t status = ProcessGroup::New(pid, {}, &process_group);
  if (status != ZX_OK) {
    return fit::error(errno(from_status_like_fdio(status)));
  }

  pids.add_process_group(process_group);

  auto task_info = task_info_factory(pid, process_group).value_or(TaskInfo{});

  process_group->insert(task_info.thread_group);

  fbl::RefPtr<Task> task;
  status = Task::New(pid, initial_name, task_info.thread_group, std::move(task_info.thread),
                     FdTable::Create(), task_info.memory_manager, root_fs, &task);
  if (status != ZX_OK) {
    return fit::error(errno(from_status_like_fdio(status)));
  }

  auto builder = TaskBuilder{task};
  // TODO (Herrera) Add fit::defer
  {
    auto temp_task = builder.task();
    auto result = builder->thread_group()->add(temp_task);
    if (result.is_error()) {
      return result.take_error();
    }

    pids.add_task(temp_task);
    pids.add_thread_group(builder->thread_group());
  }

  return fit::ok(builder);
}

fit::result<Errno> CurrentTask::exec(const FileHandle& executable, const fbl::String& path,
                                     const fbl::Vector<fbl::String>& argv,
                                     const fbl::Vector<fbl::String>& environ) {
  LTRACEF_LEVEL(2, "path=%s\n", path.c_str());
  // Executable must be a regular file
  /*
  if !executable.name.entry.node.is_reg() {
      return error!(EACCES);
  }
  */

  // File node must have EXEC mode permissions.
  // Note that the ability to execute a file is unrelated to the flags
  // used in the `open` call.
  /*
  executable.name.check_access(self, Access::EXEC)?;

  let elf_selinux_state = selinux_hooks::check_exec_access(self)?;
  */

  auto resolved_elf =
      resolve_executable(*this, executable, path, argv, environ /*,elf_selinux_state*/);
  if (resolved_elf.is_error()) {
    return resolved_elf.take_error();
  }

  {
    Guard<Mutex> lock((*this)->thread_group()->tg_rw_lock());
    if ((*this)->thread_group()->tasks_count() > 1) {
      // track_stub !(TODO("https://fxbug.dev/297434895"), "exec on multithread process");
      return fit::error(errno(EINVAL));
    }
  }

  auto err = finish_exec(path, resolved_elf.value());
  if (err.is_error()) {
    TRACEF("warning: unrecoverable error in exec: %u\n", err.error_value().error_code());
    /*
    send_standard_signal(
        self,
        SignalInfo { code: SI_KERNEL as i32, force: true, ..SignalInfo::default(SIGSEGV) },
    );
    */
    return err.take_error();
  }

  /*
    self.ptrace_event(PtraceOptions::TRACEEXEC, self.task.id as u64);
    self.signal_vfork();
  */

  return fit::ok();
}

fit::result<Errno> CurrentTask::finish_exec(const fbl::String& path,
                                            const ResolvedElf& resolved_elf) {
  LTRACEF_LEVEL(2, "path=%s\n", path.c_str());
  // Now that the exec will definitely finish (or crash), notify owners of
  // locked futexes for the current process, which will be impossible to
  // update after process image is replaced.  See get_robust_list(2).
  /*
    self.notify_robust_list();
  */

  auto exec_result = (*this)->mm()->exec();
  if (exec_result.is_error()) {
    fit::error(errno(from_status_like_fdio(exec_result.error_value())));
  }

  // Update the SELinux state, if enabled.
  /*
  selinux_hooks::update_state_on_exec(self, &resolved_elf.selinux_state);
  */

  auto start_info = load_executable(*this, resolved_elf, path);
  if (start_info.is_error()) {
    return start_info.take_error();
  }
  auto regs = zx_thread_state_general_regs_t::From(start_info.value());
  thread_state_.registers = RegisterState::From(regs);

  {
    // Guard<Mutex> lock(task_->task_mutable_state_rw_lock());
    //  task_->persistent_info()->lock();

    // state.signals.alt_stack = None;
    // state.robust_list_head = UserAddress::NULL.into();

    // TODO(tbodt): Check whether capability xattrs are set on the file, and grant/limit
    // capabilities accordingly.
    // persistent_info.creds_mut().exec();
  }

  /*
    self.thread_state.extended_pstate.reset();

    self.thread_group.signal_actions.reset_for_exec();

    // TODO(http://b/320436714): when adding SELinux support for the file subsystem, implement
    // hook to clean up state after exec.

    // TODO: The termination signal is reset to SIGCHLD.

    // TODO(https://fxbug.dev/42082680): All threads other than the calling thread are
  destroyed.

    // TODO: The file descriptor table is unshared, undoing the effect of
    //       the CLONE_FILES flag of clone(2).
    //
    // To make this work, we can put the files in an RwLock and then cache
    // a reference to the files on the CurrentTask. That will let
    // functions that have CurrentTask access the FdTable without
    // needing to grab the read-lock.
    //
    // For now, we do not implement that behavior.
    self.files.exec();

    // TODO: POSIX timers are not preserved.
  */

  {
    auto tg = task_->thread_group();
    Guard<Mutex> lock(tg->tg_rw_lock());
    tg->did_exec() = true;
  }

  // `prctl(PR_GET_NAME)` and `/proc/self/stat`
  /*
    let basename = if let Some(idx) = memchr::memrchr(b'/', path.to_bytes()) {
        // SAFETY: Substring of a CString will contain no null bytes.
        CString::new(&path.to_bytes()[idx + 1..]).unwrap()
    } else {
        path
    };
    set_zx_name(&fuchsia_runtime::thread_self(), basename.as_bytes());
    self.set_command_name(basename);
  */
  return fit::ok();
}

// This is a temporary code while we do not support file system.
// We just return the "file" VMO
fit::result<Errno, FileHandle> CurrentTask::open_file_bootfs(const fbl::String& path) {
  LTRACEF_LEVEL(2, "path=%s\n", path.c_str());

  ktl::array<zx_handle_t, userloader::kHandleCount> handles = ExtractHandles(userloader::gHandles);

  zx::unowned_vmar vmar_self = zx::vmar::root_self();

  auto [power, vmex] = CreateResources({}, handles);

  // Locate the ZBI_TYPE_STORAGE_BOOTFS item and decompress it. This will be used to load
  // the binary referenced by userboot.next, as well as libc. Bootfs will be fully parsed
  // and hosted under '/boot' either by bootsvc or component manager.
  const zx::unowned_vmo zbi{handles[userloader::kZbi]};

  auto get_bootfs_result = zbi_parser::GetBootfsFromZbi(*vmar_self, *zbi, false);
  if (get_bootfs_result.is_error()) {
    return fit::error(errno(from_status_like_fdio(get_bootfs_result.error_value())));
  }

  zx::vmo bootfs_vmo = ktl::move(get_bootfs_result.value());
  zbi_parser::Bootfs bootfs{vmar_self->borrow(), ktl::move(bootfs_vmo), ktl::move(vmex)};

  auto open_result = bootfs.Open("", path, "CurrentTask::open_file");
  if (open_result.is_error()) {
    return fit::error(errno(from_status_like_fdio(open_result.error_value())));
  }

  fbl::AllocChecker ac;
  auto fh = fbl::MakeRefCountedChecked<FileObject>(&ac, std::move(open_result.value()));
  if (!ac.check()) {
    return fit::error(errno(from_status_like_fdio(ZX_ERR_NO_MEMORY)));
  }

  return fit::ok(std::move(fh));
}

fit::result<Errno, std::pair<NamespaceNode, FsStr>> CurrentTask::resolve_dir_fd(
    FdNumber dir_fd, FsStr path, ResolveFlags flags) {
  LTRACEF_LEVEL(2, "dir_fd=%d, path=%s\n", dir_fd.raw(), path.c_str());

  bool path_is_absolute = (path.size() > 1) && path[0] == '/';
  if (path_is_absolute) {
    if (flags.contains(ResolveFlagsEnum::BENEATH)) {
      return fit::error(errno(EXDEV));
    }
    path = std::string_view(path).substr(1);
  }

  auto dir_result = [this, &path_is_absolute, &flags,
                     &dir_fd]() -> fit::result<Errno, NamespaceNode> {
    if (path_is_absolute && !flags.contains(ResolveFlagsEnum::IN_ROOT)) {
      return fit::ok((*this)->fs()->root());
    } else if (dir_fd == FdNumber::_AT_FDCWD) {
      return fit::ok((*this)->fs()->cwd());
    } else {
      // O_PATH allowed for:
      //
      //   Passing the file descriptor as the dirfd argument of
      //   openat() and the other "*at()" system calls.  This
      //   includes linkat(2) with AT_EMPTY_PATH (or via procfs
      //   using AT_SYMLINK_FOLLOW) even if the file is not a
      //   directory.
      //
      // See https://man7.org/linux/man-pages/man2/open.2.html
      auto result = (*this)->files().get_allowing_opath(dir_fd);
      if (result.is_error()) {
        return result.take_error();
      }
      return fit::ok(result.value()->name);
    }
  }();
  if (dir_result.is_error())
    return dir_result.take_error();
  auto dir = dir_result.value();

  if (!path.empty()) {
    if (!dir.entry->node->is_dir()) {
      return fit::error(errno(ENOTDIR));
    }
    if (auto check_access_result = dir.check_access(*this, Access(Access::EnumType::EXEC));
        check_access_result.is_error()) {
      return check_access_result.take_error();
    }
  }

  return fit::ok(std::make_pair(dir, path));
}

fit::result<Errno, FileHandle> CurrentTask::open_file(const FsStr& path, OpenFlags flags) {
  LTRACEF_LEVEL(2, "path=%s, flags=0x%x\n", path.c_str(), flags.bits());
  if (flags.contains(OpenFlagsEnum::CREAT)) {
    // In order to support OpenFlags::CREAT we would need to take a
    // FileMode argument.
    return fit::error(errno(EINVAL));
  }
  return open_file_at(FdNumber::_AT_FDCWD, path, flags, FileMode(), ResolveFlags::empty());
}

fit::result<Errno, std::pair<NamespaceNode, bool>> CurrentTask::resolve_open_path(
    LookupContext& context, NamespaceNode dir, const FsStr& path, FileMode mode, OpenFlags flags) {
  LTRACEF_LEVEL(2, "path=%s, flags=0x%x\n", path.c_str(), flags.bits());
  context.update_for_path(path);
  auto parent_content = context.with(SymlinkMode::Follow);
  auto lookup_parent_result = lookup_parent(parent_content, dir, path);
  if (lookup_parent_result.is_error())
    return lookup_parent_result.take_error();

  auto [parent, basename] = lookup_parent_result.value();

  context.remaining_follows = parent_content.remaining_follows;

  auto must_create = flags.contains(OpenFlagsEnum::CREAT) && flags.contains(OpenFlagsEnum::EXCL);

  // Lookup the child, without following a symlink or expecting it to be a directory.
  auto child_context = context.with(SymlinkMode::NoFollow);
  child_context.must_be_directory = false;

  if (auto lookup_child_result = parent.lookup_child(*this, child_context, basename);
      lookup_child_result.is_ok()) {
    auto name = lookup_child_result.value();
    if (name.entry->node->is_lnk()) {
      if (flags.contains(OpenFlagsEnum::PATH) && context.symlink_mode == SymlinkMode::NoFollow) {
        // When O_PATH is specified in flags, if pathname is a symbolic link
        // and the O_NOFOLLOW flag is also specified, then the call returns
        // a file descriptor referring to the symbolic link.
        // See https://man7.org/linux/man-pages/man2/openat.2.html
        //
        // If the trailing component (i.e., basename) of
        // pathname is a symbolic link, how.resolve contains
        // RESOLVE_NO_SYMLINKS, and how.flags contains both
        // O_PATH and O_NOFOLLOW, then an O_PATH file
        // descriptor referencing the symbolic link will be
        // returned.
        // See https://man7.org/linux/man-pages/man2/openat2.2.html
        return fit::ok(std::make_pair(name, false));
      }

      if ((!flags.contains(OpenFlagsEnum::PATH) && context.symlink_mode == SymlinkMode::NoFollow) ||
          context.resolve_flags.contains(ResolveFlagsEnum::NO_SYMLINKS) ||
          context.remaining_follows == 0) {
        if (must_create) {
          // Since `must_create` is set, and a node was found, this returns EEXIST
          // instead of ELOOP.
          return fit::error(errno(EEXIST));
        }
        // A symlink was found, but one of the following is true:
        // * flags specified O_NOFOLLOW but not O_PATH.
        // * how.resolve contains RESOLVE_NO_SYMLINKS
        // * too many symlink traversals have been attempted
        return fit::error(errno(ELOOP));
      }

      context.remaining_follows -= 1;

      auto readlink_result = name.readlink(*this);
      if (readlink_result.is_error())
        return readlink_result.take_error();
      return std::visit(
          SymlinkTarget::overloaded{
              [&, p = std::move(parent)](
                  const FsString& path) -> fit::result<Errno, std::pair<NamespaceNode, bool>> {
                auto dir = (path[0] == '/') ? (*this)->fs()->root() : p;
                return resolve_open_path(context, dir, path, mode, flags);
              },
              [&](NamespaceNode node) -> fit::result<Errno, std::pair<NamespaceNode, bool>> {
                if (context.resolve_flags.contains(ResolveFlagsEnum::NO_MAGICLINKS)) {
                  return fit::error(errno(ELOOP));
                }
                return fit::ok(std::make_pair(node, false));
              },
          },
          readlink_result.value().value);

    } else {
      if (must_create) {
        return fit::error(errno(EEXIST));
      }
      return fit::ok(std::make_pair(name, false));
    }
  } else {
    auto _errno = lookup_child_result.error_value();
    if ((_errno == errno(ENOENT)) && flags.contains(OpenFlagsEnum::CREAT)) {
      if (context.must_be_directory) {
        return fit::error(errno(EISDIR));
      }
      auto open_create_node_result = parent.open_create_node(
          *this, basename, mode.with_type(FileMode::IFREG), DeviceType::NONE, flags);
      if (open_create_node_result.is_error())
        return open_create_node_result.take_error();

      return fit::ok(std::make_pair(open_create_node_result.value(), true));
    } else {
      return lookup_child_result.take_error();
    }
  }
}

fit::result<Errno, FileHandle> CurrentTask::open_file_at(FdNumber dir_fd, const FsStr& path,
                                                         OpenFlags flags, FileMode mode,
                                                         ResolveFlags resolve_flags) {
  LTRACEF_LEVEL(2, "path=%s, flags=0x%x, mode=0x%x, resolve_flags=0x%x\n", path.c_str(),
                flags.bits(), mode.bits(), resolve_flags.bits());

  if (path.empty()) {
    return fit::error(errno(ENOENT));
  }

  auto result = resolve_dir_fd(dir_fd, path, resolve_flags);
  if (result.is_error()) {
    return result.take_error();
  }
  auto [dir, _path] = result.value();
  return open_namespace_node_at(dir, _path, flags, mode, resolve_flags);
}

fit::result<Errno, FileHandle> CurrentTask::open_namespace_node_at(NamespaceNode dir,
                                                                   const FsStr& path,
                                                                   OpenFlags _flags, FileMode mode,
                                                                   ResolveFlags& resolve_flags) {
  LTRACEF_LEVEL(2, "path=%s, flags=0x%x, mode=0x%x, resolve_flags=0x%x\n", path.c_str(),
                _flags.bits(), mode.bits(), resolve_flags.bits());

  // 64-bit kernels force the O_LARGEFILE flag to be on.
  OpenFlagsImpl flags(_flags | OpenFlagsEnum::LARGEFILE);

  if (flags.contains(OpenFlagsEnum::PATH)) {
    // When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
    // O_DIRECTORY, and O_NOFOLLOW are ignored.
    const OpenFlags ALLOWED_FLAGS = OpenFlags::from_bits_truncate(
        OpenFlags(OpenFlagsEnum::PATH).bits() | OpenFlags(OpenFlagsEnum::CLOEXEC).bits() |
        OpenFlags(OpenFlagsEnum::DIRECTORY).bits() | OpenFlags(OpenFlagsEnum::NOFOLLOW).bits());

    flags &= ALLOWED_FLAGS;
  }

  if (flags.contains(OpenFlagsEnum::TMPFILE) && !flags.can_write()) {
    return fit::error(errno(EINVAL));
  }

  bool nofollow = flags.contains(OpenFlagsEnum::NOFOLLOW);
  bool must_create = flags.contains(OpenFlagsEnum::CREAT) && flags.contains(OpenFlagsEnum::EXCL);

  auto symlink_mode = (nofollow || must_create) ? SymlinkMode::NoFollow : SymlinkMode::Follow;

  // Define the resolve_base variable
  ResolveBase resolve_base;
  bool beneath = resolve_flags.contains(ResolveFlagsEnum::BENEATH);
  bool in_root = resolve_flags.contains(ResolveFlagsEnum::IN_ROOT);

  if (!beneath && !in_root) /*(false, false)*/ {
    resolve_base = {ResolveBaseType::None, NamespaceNode()};
  } else if (beneath && !in_root) /*(true, false)*/ {
    resolve_base = {ResolveBaseType::Beneath, dir};
  } else if (!beneath && in_root) /* (false, true)*/ {
    resolve_base = {ResolveBaseType::InRoot, dir};
  } else {
    // Both flags are true, return error
    return fit::error(errno(EINVAL));
  }

  // `RESOLVE_BENEATH` and `RESOLVE_IN_ROOT` imply `RESOLVE_NO_MAGICLINKS`. This matches
  // Linux behavior. Strictly speaking it's is not really required, but it's hard to
  // implement `BENEATH` and `IN_ROOT` flags correctly otherwise.

  if (resolve_base.type != ResolveBaseType::None) {
    resolve_flags.insert(ResolveFlagsEnum::NO_MAGICLINKS);
  }

  auto context = LookupContext{
      symlink_mode,  MAX_SYMLINK_FOLLOWS, flags.contains(OpenFlagsEnum::DIRECTORY),
      resolve_flags, resolve_base,
  };

  auto result = resolve_open_path(context, dir, path, mode, flags);
  if (result.is_error()) {
    /*
      let mut abs_path = dir.path(&self.task);
      abs_path.extend(&**path);
      track_file_not_found(abs_path);
      return Err(e);
    */
    return result.take_error();
  }

  auto [name, created] = result.value();

  auto name_result = [&, name_ = std::move(name),
                      created_ = created]() -> fit::result<Errno, NamespaceNode> {
    if (flags.contains(OpenFlagsEnum::TMPFILE)) {
      return name_.create_tmpfile(*this, mode.with_type(FileMode::IFREG), flags);
    } else {
      auto mode_ = name_.entry->node->info()->mode;

      // These checks are not needed in the `O_TMPFILE` case because `mode` refers to the
      // file we are opening. With `O_TMPFILE`, that file is the regular file we just
      // created rather than the node we found by resolving the path.
      //
      // For example, we do not need to produce `ENOTDIR` when `must_be_directory` is set
      // because `must_be_directory` refers to the node we found by resolving the path.
      // If that node was not a directory, then `create_tmpfile` will produce an error.
      //
      // Similarly, we never need to call `truncate` because `O_TMPFILE` is newly created
      // and therefor already an empty file.

      if (nofollow && mode_.is_lnk()) {
        return fit::error(errno(ELOOP));
      }

      if (mode.is_dir()) {
      } else if (context.must_be_directory) {
        return fit::error(errno(ENOTDIR));
      }

      if (flags.contains(OpenFlagsEnum::TRUNC) && mode.is_reg() && !created_) {
        // You might think we should check file.can_write() at this
        // point, which is what the docs suggest, but apparently we
        // are supposed to truncate the file if this task can write
        // to the underlying node, even if we are opening the file
        // as read-only. See OpenTest.CanTruncateReadOnly.
        if (auto truncate_result = name_.truncate(*this, 0); truncate_result.is_error()) {
          return truncate_result.take_error();
        }
      }
      return fit::ok(name_);
    }
  }();

  if (name_result.is_error())
    return name_result.take_error();

  // If the node has been created, the open operation should not verify access right:
  // From <https://man7.org/linux/man-pages/man2/open.2.html>
  //
  // > Note that mode applies only to future accesses of the newly created file; the
  // > open() call that creates a read-only file may well return a  read/write  file
  // > descriptor.

  auto _name = name_result.value();
  return _name.open(*this, flags, !created);
}

fit::result<Errno, std::pair<NamespaceNode, FsString>> CurrentTask::lookup_parent_at() const {
  return fit::error(errno(EINVAL));
}

fit::result<Errno, std::pair<NamespaceNode, FsString>> CurrentTask::lookup_parent(
    LookupContext& context, const NamespaceNode& dir, const FsStr& path) const {
  context.update_for_path(path);

  auto current_node = dir;
  auto split = SplitStringCopy(path, "/", kTrimWhitespace, kSplitWantNonEmpty);
  auto it = split.begin();
  FsString current_path_component = (it != split.end()) ? *it++ : "";
  for (; it != split.end(); ++it) {
    if (auto lookup_child_result =
            current_node.lookup_child(*this, context, current_path_component);
        lookup_child_result.is_error()) {
      return lookup_child_result.take_error();
    } else {
      current_path_component = *it;
    }
  }
  return fit::ok(std::make_pair(current_node, current_path_component));
}

fit::result<Errno, NamespaceNode> CurrentTask::lookup_path(LookupContext& context,
                                                           NamespaceNode dir,
                                                           const FsStr& path) const {
  LTRACEF_LEVEL(2, "path=%s\n", path.c_str());

  auto lookup_parent_result = lookup_parent(context, dir, path);
  if (lookup_parent_result.is_error())
    return lookup_parent_result.take_error();
  auto [parent, basename] = lookup_parent_result.value();
  return parent.lookup_child(*this, context, basename);
}

fit::result<Errno, NamespaceNode> CurrentTask::lookup_path_from_root(const FsStr& path) const {
  LTRACEF_LEVEL(2, "path=%s\n", path.c_str());

  LookupContext context = LookupContext::Default();
  return lookup_path(context, (*this)->fs()->root(), path);
}

fit::result<Errno, ktl::span<uint8_t>> CurrentTask::read_memory(UserAddress addr,
                                                                ktl::span<uint8_t>& bytes) const {
  return task_->mm()->unified_read_memory(*this, addr, bytes);
}

fit::result<Errno, size_t> CurrentTask::write_memory(UserAddress addr,
                                                     const ktl::span<const uint8_t>& bytes) const {
  return task_->mm()->unified_write_memory(*this, addr, bytes);
}

}  // namespace starnix
