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
    use std::time::Duration;
    use zerocopy::{FromBytes, IntoBytes};

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

    fn bpf_map_update_elem(
        map_fd: BorrowedFd<'_>,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let update_elem_attr = unsafe { &mut attr.__bindgen_anon_2 };
        update_elem_attr.map_fd = map_fd.as_raw_fd() as u32;
        update_elem_attr.key = key.as_ptr() as u64;

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let value_field = &mut update_elem_attr.__bindgen_anon_1;
        value_field.value = value.as_ptr() as u64;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_MAP_UPDATE_ELEM, &attr) }.map(|_| ())
    }

    fn bpf_prog_load(
        code: Vec<ebpf::EbpfInstruction>,
        prog_type: linux_uapi::bpf_prog_type,
        expected_attach_type: linux_uapi::bpf_attach_type,
    ) -> Result<OwnedFd, std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let load_prog_attr = unsafe { &mut attr.__bindgen_anon_3 };
        load_prog_attr.prog_type = prog_type;
        load_prog_attr.insns = code.as_ptr() as u64;
        load_prog_attr.insn_cnt = code.len() as u32;
        load_prog_attr.expected_attach_type = expected_attach_type;
        load_prog_attr.log_level = 1;
        load_prog_attr.log_size = 65536;

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
            ()
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

    // Names of the eBPF maps defined in `ebpf_test_progs.c`.
    const RINGBUF_MAP_NAME: &str = "ringbuf";
    const TARGET_COOKIE_MAP_NAME: &str = "target_cookie";

    struct MapSet {
        maps: Vec<(MapDefinition, OwnedFd)>,
    }

    impl MapSet {
        fn find(&self, name: &str, expected_type: linux_uapi::bpf_map_type) -> BorrowedFd<'_> {
            let (def, fd) = self
                .maps
                .iter()
                .find(|(def, _fd)| def.name == bstr::BStr::new(name))
                .unwrap_or_else(|| panic!("Failed to find map {}", name));
            assert!(def.schema.map_type == expected_type, "Invalid map type for map {}", name);
            fd.as_fd()
        }

        fn ringbuf(&self) -> BorrowedFd<'_> {
            self.find(RINGBUF_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_RINGBUF)
        }

        fn set_target_cookie(&self, cookie: u64) {
            let target_cookie_fd =
                self.find(TARGET_COOKIE_MAP_NAME, linux_uapi::bpf_map_type_BPF_MAP_TYPE_ARRAY);
            let key: u32 = 0;
            bpf_map_update_elem(target_cookie_fd, key.as_bytes(), cookie.as_bytes())
                .expect("Failed to set target_cookie");
        }
    }

    struct LoadedProgram {
        prog_fd: OwnedFd,
        maps: MapSet,
    }

    fn load_program(name: &str, attach_type: linux_uapi::bpf_attach_type) -> LoadedProgram {
        let ProgramDefinition { mut code, maps: map_defs } =
            ebpf_loader::load_ebpf_program("data/ebpf/ebpf_test_progs.o", ".text", name)
                .expect("Failed to load program");

        let map_fds: Vec<_> = map_defs
            .iter()
            .map(|map_def| bpf_map_create(map_def).expect("Failed to create map"))
            .collect();

        // Replace map indices with FDs.
        for inst in code.iter_mut() {
            if inst.code() == ebpf::BPF_LDDW {
                if inst.src_reg() == ebpf::BPF_PSEUDO_MAP_IDX {
                    let map_index = inst.imm() as usize;
                    let map_fd = map_fds[map_index].as_raw_fd();
                    inst.set_src_reg(ebpf::BPF_PSEUDO_MAP_FD);
                    inst.set_imm(map_fd);
                }
            }
        }

        let prog_fd =
            bpf_prog_load(code, linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB, attach_type)
                .expect("Failed to load program");

        let maps = map_defs.into_iter().zip(map_fds.into_iter()).collect();
        LoadedProgram { prog_fd, maps: MapSet { maps } }
    }

    struct AttachedProgram {
        attach_type: linux_uapi::bpf_attach_type,
        cgroup: File,
    }

    impl AttachedProgram {
        fn new(fd: OwnedFd, attach_type: linux_uapi::bpf_attach_type) -> Self {
            let cgroup = File::open("/sys/fs/cgroup").expect("Failed to open cgroup");
            bpf_prog_attach(attach_type, cgroup.as_fd(), fd.as_fd())
                .expect("Failed to attach program");
            AttachedProgram { attach_type, cgroup }
        }
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

        let LoadedProgram { prog_fd, maps } =
            load_program("skb_test_prog", linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS);

        // Check that the ring buffer is not signalled initially.
        let signaled = pollfd(maps.ringbuf(), libc::POLLIN, Duration::ZERO)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == None);

        let socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        let cookie = get_socket_cookie(socket.as_fd()).expect("Failed to get SO_COOKIE");
        maps.set_target_cookie(cookie);

        // Attach the program.
        let _attached =
            AttachedProgram::new(prog_fd, linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS);

        // Send a UDP packet.
        socket.send_to(&[1, 2, 3], "127.0.0.1:12345").expect("Failed to send UDP packet");

        // The ring buffer FD should be signalled by the program.
        let signaled = pollfd(maps.ringbuf(), libc::POLLIN, Duration::MAX)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == Some(libc::POLLIN));
    }

    #[test]
    fn ebpf_ingress() {
        root_required!();

        let LoadedProgram { prog_fd, maps } =
            load_program("skb_test_prog", linux_uapi::bpf_attach_type_BPF_CGROUP_INET_INGRESS);

        // Setup a listening socket.
        let recv_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        let recv_addr = recv_socket.local_addr().expect("Failed to get local socket addr");

        let cookie = get_socket_cookie(recv_socket.as_fd()).expect("Failed to get SO_COOKIE");
        maps.set_target_cookie(cookie);

        // Attach the program.
        let _attached =
            AttachedProgram::new(prog_fd, linux_uapi::bpf_attach_type_BPF_CGROUP_INET_INGRESS);

        // Send a UDP packet.
        let send_socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        send_socket.send_to(&[1, 2, 3], recv_addr).expect("Failed to send UDP packet");

        // The ring buffer FD should be signalled by the program.
        let signaled = pollfd(maps.ringbuf(), libc::POLLIN, Duration::MAX)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == Some(libc::POLLIN));
    }
}
