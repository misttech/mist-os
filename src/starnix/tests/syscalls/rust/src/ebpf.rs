// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod tests {
    use ebpf_loader::{MapDefinition, ProgramDefinition};
    use libc;
    use linux_uapi::bpf_attr;
    use std::fs::File;
    use std::net::UdpSocket;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use zerocopy::{FromBytes, Immutable, IntoBytes};

    macro_rules! root_required {
        () => {
            // geteuid() is always safe to call.
            let euid = unsafe { libc::geteuid() };
            if euid != 0 {
                eprintln!("eBPF tests require root privileges, skipping");
                return;
            }
        };
    }

    fn zero_bpf_attr() -> bpf_attr {
        bpf_attr::read_from_bytes(&[0; std::mem::size_of::<bpf_attr>()])
            .expect("Failed to create bpf_attr")
    }

    unsafe fn bpf(command: linux_uapi::bpf_cmd, attr: &bpf_attr) -> Result<i32, std::io::Error> {
        let result = libc::syscall(
            linux_uapi::__NR_bpf.into(),
            command,
            attr as *const bpf_attr,
            std::mem::size_of_val(attr),
        );
        (result >= 0)
            .then_some(result as i32)
            .ok_or(std::io::Error::from_raw_os_error(-result as i32))
    }

    fn gettid() -> linux_uapi::pid_t {
        // SAFETY: gettid syscall is always safe.
        unsafe { libc::syscall(linux_uapi::__NR_gettid.into()) as linux_uapi::pid_t }
    }

    fn bpf_map_create(map_def: &ebpf_loader::MapDefinition) -> Result<OwnedFd, std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let create_map_attr = unsafe { &mut attr.__bindgen_anon_1 };
        create_map_attr.map_type = map_def.schema.map_type;
        create_map_attr.key_size = map_def.schema.key_size;
        create_map_attr.value_size = map_def.schema.value_size;
        create_map_attr.max_entries = map_def.schema.max_entries;
        create_map_attr.map_flags = map_def.flags;

        // SAFETY: `bpf()` syscall with valid arguments.
        let result = unsafe { bpf(linux_uapi::bpf_cmd_BPF_MAP_CREATE, &attr) };

        // SAFETY: result is an FD when non-negative.
        result.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn bpf_map_update_elem<K: IntoBytes + Immutable, V: IntoBytes + Immutable>(
        map_fd: BorrowedFd<'_>,
        key: K,
        value: V,
    ) -> Result<(), std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let update_elem_attr = unsafe { &mut attr.__bindgen_anon_2 };
        update_elem_attr.map_fd = map_fd.as_raw_fd() as u32;
        update_elem_attr.key = key.as_bytes().as_ptr() as u64;

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let value_field = &mut update_elem_attr.__bindgen_anon_1;
        value_field.value = value.as_bytes().as_ptr() as u64;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_MAP_UPDATE_ELEM, &attr) }.map(|r| {
            assert!(r == 0);
        })
    }

    fn bpf_map_lookup_elem<K: IntoBytes + Immutable, T: FromBytes>(
        map_fd: BorrowedFd<'_>,
        key: K,
    ) -> Result<T, std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let update_elem_attr = unsafe { &mut attr.__bindgen_anon_2 };
        update_elem_attr.map_fd = map_fd.as_raw_fd() as u32;
        update_elem_attr.key = key.as_bytes().as_ptr() as u64;

        let mut value = vec![0; std::mem::size_of::<T>()];

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let value_field = &mut update_elem_attr.__bindgen_anon_1;
        value_field.value = value.as_mut_ptr() as u64;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_MAP_LOOKUP_ELEM, &attr) }.map(|r| {
            assert!(r == 0);
            T::read_from_bytes(&value).expect("Failed to convert lookup result")
        })
    }

    fn bpf_prog_load(
        code: Vec<ebpf::EbpfInstruction>,
        prog_type: linux_uapi::bpf_prog_type,
        expected_attach_type: linux_uapi::bpf_attach_type,
    ) -> Result<OwnedFd, std::io::Error> {
        let mut attr = zero_bpf_attr();

        let mut log = vec![0; 4096];
        let license = b"N/A\0";

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let load_prog_attr = unsafe { &mut attr.__bindgen_anon_3 };
        load_prog_attr.prog_type = prog_type;
        load_prog_attr.insns = code.as_ptr() as u64;
        load_prog_attr.insn_cnt = code.len() as u32;
        load_prog_attr.expected_attach_type = expected_attach_type;
        load_prog_attr.log_level = 1;
        load_prog_attr.log_size = 4096;
        load_prog_attr.log_buf = log.as_mut_ptr() as u64;
        load_prog_attr.license = license.as_ptr() as u64;

        // SAFETY: `bpf()` syscall with valid arguments.
        let result = unsafe { bpf(linux_uapi::bpf_cmd_BPF_PROG_LOAD, &attr) };

        // SAFETY: result is an FD when non-negative.
        result.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn bpf_prog_attach(
        attach_type: linux_uapi::bpf_attach_type,
        attach_target: BorrowedFd<'_>,
        prog_fd: BorrowedFd<'_>,
    ) -> Result<(), std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let attach_prog_attr = unsafe { &mut attr.__bindgen_anon_5 };
        attach_prog_attr.attach_bpf_fd = prog_fd.as_raw_fd() as u32;
        attach_prog_attr.attach_type = attach_type;
        attach_prog_attr.__bindgen_anon_1.target_fd = attach_target.as_raw_fd() as u32;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_PROG_ATTACH, &attr) }.map(|r| {
            assert!(r == 0);
        })
    }

    fn bpf_prog_detach(
        attach_type: linux_uapi::bpf_attach_type,
        attach_target: BorrowedFd<'_>,
    ) -> Result<(), std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let attach_prog_attr = unsafe { &mut attr.__bindgen_anon_5 };
        attach_prog_attr.attach_type = attach_type;
        attach_prog_attr.__bindgen_anon_1.target_fd = attach_target.as_raw_fd() as u32;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_PROG_DETACH, &attr) }.map(|r| {
            assert!(r == 0);
            ()
        })
    }

    fn pollfd(
        fd: BorrowedFd<'_>,
        events: libc::c_short,
        timeout: Duration,
    ) -> Result<Option<libc::c_short>, std::io::Error> {
        let mut fds = [libc::pollfd { fd: fd.as_raw_fd(), events, revents: 0 }];

        // If the specified timeout is greater than i32::MAX milliseconds then
        // pass -1 to `poll()` to wait indefinitely.
        let timeout_ms = timeout.as_millis().try_into().unwrap_or(-1);

        // SAFETY: poll is safe to call with a valid pollfd array.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };

        if result < 0 {
            Err(std::io::Error::last_os_error())
        } else if result > 0 {
            Ok(Some(fds[0].revents))
        } else {
            Ok(None)
        }
    }

    fn get_socket_cookie(fd: BorrowedFd<'_>) -> Result<u64, std::io::Error> {
        let mut value: u64 = 0;
        let mut value_len: libc::socklen_t = std::mem::size_of_val(&value) as u32;
        // SAFETY: `getsockopt()` call with valid arguments.
        let result = unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_COOKIE,
                &mut value as *mut u64 as *mut libc::c_void,
                &mut value_len,
            )
        };

        if result < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            assert!(value_len == std::mem::size_of_val(&value) as u32);
            Ok(value)
        }
    }

    fn setsockopt(
        fd: BorrowedFd<'_>,
        level: libc::c_int,
        optname: libc::c_int,
        value: &[u8],
    ) -> Result<(), std::io::Error> {
        // SAFETY: `setsockopt()` call with valid arguments.
        let result = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                level,
                optname,
                value.as_ptr() as *mut libc::c_void,
                value.len() as libc::socklen_t,
            )
        };

        (result >= 0).then_some(()).ok_or(std::io::Error::last_os_error())
    }

    fn getsockopt(
        fd: BorrowedFd<'_>,
        level: libc::c_int,
        optname: libc::c_int,
        buffer_size: usize,
    ) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = vec![0; buffer_size];
        let mut optlen = buffer_size as libc::socklen_t;
        // SAFETY: `setsockopt()` call with valid arguments.
        let result = unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                level,
                optname,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut optlen as *mut libc::socklen_t,
            )
        };

        (result >= 0)
            .then(|| {
                buf.resize(optlen as usize, 0);
                buf
            })
            .ok_or(std::io::Error::last_os_error())
    }

    fn getpagesize() -> usize {
        // SAFETY: `sysconf()` is safe to call.
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
    }

    // Names of the eBPF maps defined in `ebpf_test_progs.c`.
    const RINGBUF_MAP_NAME: &str = "ringbuf";
    const TARGET_COOKIE_MAP_NAME: &str = "target_cookie";
    const COUNT_MAP_NAME: &str = "count";
    const TEST_RESULT_MAP_NAME: &str = "test_result";

    // LINT.IfChange
    #[repr(C)]
    #[derive(Immutable, FromBytes)]
    struct TestResult {
        uid_gid: u64,
        pid_tgid: u64,

        optlen: u64,
        optval_size: u64,
    }

    #[repr(C)]
    #[derive(Debug, Immutable, FromBytes)]
    struct GlobalVariables {
        global_counter1: u64,
        global_counter2: u64,
    }

    const TEST_SOCK_OPT: libc::c_int = 12345;
    // LINT.ThenChange(//src/starnix/tests/syscalls/rust/data/ebpf/ebpf_test_progs.c)

    struct MapSet {
        maps: Vec<(MapDefinition, OwnedFd)>,
    }

    impl MapSet {
        fn find(&self, name: &str, expected_type: linux_uapi::bpf_map_type) -> BorrowedFd<'_> {
            let (def, fd) = self
                .maps
                .iter()
                .find(|(def, _fd)| {
                    def.name.as_ref().map(|x| x == bstr::BStr::new(name)).unwrap_or(false)
                })
                .unwrap_or_else(|| panic!("Failed to find map {}", name));
            assert!(def.schema.map_type == expected_type, "Invalid map type for map {}", name);
            fd.as_fd()
        }

        fn rss(&self) -> BorrowedFd<'_> {
            let (def, fd) = self
                .maps
                .iter()
                .find(|(def, _fd)| def.name.is_none())
                .unwrap_or_else(|| panic!("Failed to find rss map"));
            assert!(
                def.schema.map_type == linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY,
                "Invalid map type for map rss"
            );
            fd.as_fd()
        }

        fn ringbuf(&self) -> BorrowedFd<'_> {
            self.find(RINGBUF_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_RINGBUF)
        }

        fn set_target_cookie(&self, cookie: u64) {
            let target_cookie_fd =
                self.find(TARGET_COOKIE_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY);
            bpf_map_update_elem(target_cookie_fd, 0u32, cookie)
                .expect("Failed to set target_cookie");
        }

        fn get_count(&self) -> u32 {
            let count_map_fd =
                self.find(COUNT_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY);
            bpf_map_lookup_elem(count_map_fd, 0u32).expect("Failed to get count")
        }

        fn get_test_result(&self) -> TestResult {
            let test_result_map_fd =
                self.find(TEST_RESULT_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY);
            bpf_map_lookup_elem(test_result_map_fd, 0u32).expect("Failed to test_result")
        }

        fn get_global_variables(&self) -> GlobalVariables {
            let rss_map_fd = self.rss();
            bpf_map_lookup_elem(rss_map_fd, 0u32).expect("Failed to test_result")
        }
    }

    struct LoadedProgram {
        prog_fd: OwnedFd,
        maps: MapSet,
        attach_type: linux_uapi::bpf_attach_type,
    }

    impl LoadedProgram {
        fn new(
            name: &str,
            prog_type: linux_uapi::bpf_prog_type,
            attach_type: linux_uapi::bpf_attach_type,
        ) -> Self {
            let ProgramDefinition { mut code, maps: map_defs } =
                ebpf_loader::load_ebpf_program("data/ebpf/ebpf_test_progs.o", ".text", name)
                    .expect("Failed to load program");

            let map_fds: Vec<_> = map_defs
                .iter()
                .map(|map_def| bpf_map_create(map_def).expect("Failed to create map"))
                .collect();

            // Replace map indices with FDs.
            for inst in code.iter_mut() {
                if inst.code() == ebpf::BPF_LDDW && inst.src_reg() == ebpf::BPF_PSEUDO_MAP_IDX {
                    let map_index = inst.imm() as usize;
                    let map_fd = map_fds[map_index].as_raw_fd();
                    inst.set_src_reg(ebpf::BPF_PSEUDO_MAP_FD);
                    inst.set_imm(map_fd);
                }
                if inst.code() == ebpf::BPF_LDDW && inst.src_reg() == ebpf::BPF_PSEUDO_MAP_IDX_VALUE
                {
                    let map_index = inst.imm() as usize;
                    let map_fd = map_fds[map_index].as_raw_fd();
                    inst.set_src_reg(ebpf::BPF_PSEUDO_MAP_VALUE);
                    inst.set_imm(map_fd);
                }
            }

            let prog_fd =
                bpf_prog_load(code, prog_type, attach_type).expect("Failed to load program");

            let maps = map_defs.into_iter().zip(map_fds.into_iter()).collect();
            Self { prog_fd, maps: MapSet { maps }, attach_type }
        }

        fn attach(&self) -> AttachedProgram {
            let cgroup = File::open("/sys/fs/cgroup").expect("Failed to open cgroup");
            bpf_prog_attach(self.attach_type, cgroup.as_fd(), self.prog_fd.as_fd())
                .expect("Failed to attach program");
            AttachedProgram { attach_type: self.attach_type, cgroup }
        }
    }

    struct AttachedProgram {
        attach_type: linux_uapi::bpf_attach_type,
        cgroup: File,
    }

    impl Drop for AttachedProgram {
        fn drop(&mut self) {
            let Self { attach_type, cgroup } = self;
            bpf_prog_detach(*attach_type, cgroup.as_fd()).expect("Failed to detach program");
        }
    }

    #[test]
    fn ebpf_egress() {
        root_required!();

        let program = LoadedProgram::new(
            "skb_test_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB,
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS,
        );

        // Check that the ring buffer is not signalled initially.
        let signaled = pollfd(program.maps.ringbuf(), libc::POLLIN, Duration::ZERO)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == None);

        let socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        let cookie = get_socket_cookie(socket.as_fd()).expect("Failed to get SO_COOKIE");
        program.maps.set_target_cookie(cookie);

        let _attached = program.attach();

        // Send a UDP packet.
        socket.send_to(&[1, 2, 3], "127.0.0.1:12345").expect("Failed to send UDP packet");

        // The ring buffer FD should be signalled by the program.
        let signaled = pollfd(program.maps.ringbuf(), libc::POLLIN, Duration::MAX)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == Some(libc::POLLIN));
    }

    #[test]
    fn ebpf_ingress() {
        root_required!();

        let program = LoadedProgram::new(
            "skb_test_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB,
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_INGRESS,
        );

        // Setup a listening socket.
        let recv_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        let recv_addr = recv_socket.local_addr().expect("Failed to get local socket addr");

        let cookie = get_socket_cookie(recv_socket.as_fd()).expect("Failed to get SO_COOKIE");
        program.maps.set_target_cookie(cookie);

        let _attached = program.attach();

        // Send a UDP packet.
        let send_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        send_socket.send_to(&[1, 2, 3], recv_addr).expect("Failed to send UDP packet");

        // The ring buffer FD should be signalled by the program.
        let signaled = pollfd(program.maps.ringbuf(), libc::POLLIN, Duration::MAX)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == Some(libc::POLLIN));
    }

    #[test]
    fn ebpf_sock_create() {
        root_required!();

        let program = LoadedProgram::new(
            "sock_create_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK,
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_SOCK_CREATE,
        );
        let _attached = program.attach();

        // Verify that the counter is incremented when a new socket is created.
        let last_count = program.maps.get_count();
        let initial_variable = program.maps.get_global_variables();

        let _socket = UdpSocket::bind("0.0.0.0:0").expect("Failed to create UDP socket");

        let new_count = program.maps.get_count();
        let end_variable = program.maps.get_global_variables();

        assert!(new_count - last_count >= 1);
        assert!(end_variable.global_counter1 - initial_variable.global_counter1 >= 1);
        assert!(
            end_variable.global_counter2 - initial_variable.global_counter2
                >= 2 * (end_variable.global_counter1 - initial_variable.global_counter1)
        );
    }

    #[test]
    fn sock_release_prog() {
        root_required!();

        let program = LoadedProgram::new(
            "sock_release_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK,
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_SOCK_RELEASE,
        );

        let socket = UdpSocket::bind("0.0.0.0:0").expect("Failed to create UDP socket");
        let cookie = get_socket_cookie(socket.as_fd()).expect("Failed to get SO_COOKIE");
        program.maps.set_target_cookie(cookie);

        let _attached = program.attach();

        // Verify that the counter is incremented when a new socket is released.
        let last_count = program.maps.get_count();
        std::mem::drop(socket);
        let new_count = program.maps.get_count();
        assert_eq!(new_count, last_count + 1);

        let test_result = program.maps.get_test_result();

        // SAFETY: These libc functions are safe to call.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        assert_eq!(test_result.uid_gid, (uid as u64) + (gid as u64) << 32);

        let tid = gettid();
        let tgid = std::process::id();
        assert_eq!(test_result.pid_tgid, (tid as u64) + (tgid as u64) << 32);
    }

    #[test]
    fn ebpf_setsockopt() {
        root_required!();

        let program = LoadedProgram::new(
            "setsockopt_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCKOPT,
            linux_uapi::bpf_attach_type_BPF_CGROUP_SETSOCKOPT,
        );
        let _attached = program.attach();

        let (socket, _peer) = UnixStream::pair().expect("Failed to create UNIX socket");

        let set_test_sock_opt =
            |optval| setsockopt(socket.as_fd(), libc::SOL_SOCKET, TEST_SOCK_OPT, optval);

        // Set TEST_SOCK_OPT.
        let optval = vec![0; 10000];
        assert!(set_test_sock_opt(&optval).is_ok());

        // Since the `optval` is larger than one page the program will get
        // only the first page.
        let test_result = program.maps.get_test_result();
        assert_eq!(test_result.optlen, 10000);
        assert_eq!(test_result.optval_size, getpagesize() as u64);

        // Try again with the `optval[0]=1`. The program will set `optlen`
        // above buffer size, which should result in `EFAULT`.
        let optval = vec![1; 10];
        let err = set_test_sock_opt(&optval).expect_err("setsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::EFAULT));

        // The program rejects calls with `optval[0]=2`. `EPERM` is expected.
        let optval = vec![2; 10];
        let err = set_test_sock_opt(&optval).expect_err("setsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::EPERM));

        let set_sndbuf =
            |optval| setsockopt(socket.as_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, optval);

        let get_rcvbuf = || {
            let v = getsockopt(socket.as_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, 4)
                .expect("getsockopt failed");
            libc::socklen_t::read_from_bytes(&v).expect("getsockopt returned invalid result")
        };

        // Try setting an option that's not handled by the program.
        let buffer_size: libc::socklen_t = 4096;
        assert!(set_sndbuf(buffer_size.as_bytes()).is_ok());
        assert_eq!(get_rcvbuf(), buffer_size * 2);

        // Try the same option with a larger buffer.
        let mut large_optval = vec![0; 10000];
        large_optval[0..4].copy_from_slice(buffer_size.as_bytes());
        assert!(set_sndbuf(&large_optval).is_ok());
        assert_eq!(get_rcvbuf(), buffer_size * 2);

        // The test program overrides rcvbuf=55555 with rcvbuf=66666.
        let buffer_size: libc::socklen_t = 55555;
        assert!(set_sndbuf(buffer_size.as_bytes()).is_ok());
        assert_eq!(get_rcvbuf(), 66666 * 2);
    }

    #[test]
    fn ebpf_getsockopt() {
        root_required!();

        let program = LoadedProgram::new(
            "getsockopt_prog",
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCKOPT,
            linux_uapi::bpf_attach_type_BPF_CGROUP_GETSOCKOPT,
        );
        let _attached = program.attach();

        let (socket, _peer) = UnixStream::pair().expect("Failed to create UNIX socket");

        let get_test_sock_opt =
            |optlen| getsockopt(socket.as_fd(), libc::SOL_SOCKET, TEST_SOCK_OPT, optlen);

        // If the syscall fails and eBPF doesn't change `retval` then the
        // original error is returned.
        let err = get_test_sock_opt(2).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOPROTOOPT));

        // The original error is still returned if the program returns 0.
        let err = get_test_sock_opt(3).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOPROTOOPT));

        // eBPF program can override the result.
        let result = get_test_sock_opt(4).expect("getsockopt failed");
        assert_eq!(result, vec![42, 0, 0, 0]);

        // `optlen` cannot be set to -1.
        let err = get_test_sock_opt(5).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::ENOPROTOOPT));

        let get_rcvbuf =
            |optlen| getsockopt(socket.as_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, optlen);

        // Verify that eBPF program can override the returned value.
        let result = get_rcvbuf(55).expect("getsockopt failed");
        assert_eq!(result.len(), 8);
        assert_eq!(u64::from_ne_bytes(result.try_into().unwrap()), 0x1234567890abcdef);

        // EPERM is returned if the program rejects the call by returning 0.
        let err = get_rcvbuf(56).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::EPERM));

        // EFAULT is returned to the user if the program set `optlen` to -1.
        let err = get_rcvbuf(57).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(libc::EFAULT));

        // If the program changes retval then it's returned to the userspace.
        let err = get_rcvbuf(58).expect_err("getsockopt expected to fail");
        assert_eq!(err.raw_os_error(), Some(5));
    }
}
