// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod tests {
    use ebpf_loader::ProgramDefinition;
    use libc;
    use linux_uapi::bpf_attr;
    use std::net::UdpSocket;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use std::time::Duration;
    use zerocopy::FromBytes;

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
    ) -> Result<i32, std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let attach_prog_attr = unsafe { &mut attr.__bindgen_anon_5 };
        attach_prog_attr.attach_bpf_fd = prog_fd.as_raw_fd() as u32;
        attach_prog_attr.attach_type = attach_type;
        attach_prog_attr.__bindgen_anon_1.target_fd = attach_target.as_raw_fd() as u32;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_PROG_ATTACH, &attr) }
    }

    fn bpf_prog_detach(
        attach_type: linux_uapi::bpf_attach_type,
        attach_target: BorrowedFd<'_>,
    ) -> Result<i32, std::io::Error> {
        let mut attr = zero_bpf_attr();

        // SAFETY: `attr` is zeroed, so it's safe to access any union variant.
        let attach_prog_attr = unsafe { &mut attr.__bindgen_anon_5 };
        attach_prog_attr.attach_type = attach_type;
        attach_prog_attr.__bindgen_anon_1.target_fd = attach_target.as_raw_fd() as u32;

        // SAFETY: `bpf()` syscall with valid arguments.
        unsafe { bpf(linux_uapi::bpf_cmd_BPF_PROG_DETACH, &attr) }
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

    #[test]
    fn ebpf_egress() {
        // geteuid() is always safe to call.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            eprintln!("eBPF tests require root privileges, skipping");
            return;
        }

        let ProgramDefinition { mut code, maps } = ebpf_loader::load_ebpf_program(
            "data/ebpf/ebpf_test_progs.o",
            ".text",
            "egress_test_prog",
        )
        .expect("Failed to load program");

        let map_fds: Vec<_> = maps
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

        let prog_fd = bpf_prog_load(
            code,
            linux_uapi::bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB,
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS,
        )
        .expect("Failed to load program");
        let root_cgroup = std::fs::File::open("/sys/fs/cgroup").expect("Failed to open cgroup");

        // Check that the ring buffer is not signalled initially.
        assert!(maps[0].schema.map_type == linux_uapi::bpf_map_type_BPF_MAP_TYPE_RINGBUF);
        let ringbuf_fd = map_fds[0].as_fd();
        let signaled = pollfd(ringbuf_fd.as_fd(), libc::POLLIN, Duration::ZERO)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == None);

        // Attach the program.
        let _attach_result = bpf_prog_attach(
            linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS,
            root_cgroup.as_fd(),
            prog_fd.as_fd(),
        )
        .expect("Failed to attach program");

        // Send a UDP packet.
        let socket = UdpSocket::bind("127.0.0.1:0").expect("Failed ot create UPD socket");
        socket.send_to(&[1, 2, 3], "127.0.0.1:12345").expect("Failed to send UDP packet");

        // The ring buffer FD should be signalled by the program.
        let signaled = pollfd(ringbuf_fd.as_fd(), libc::POLLIN, Duration::MAX)
            .expect("Failed to poll ringbuffer FD");
        assert!(signaled == Some(libc::POLLIN));

        // Detach the program.
        bpf_prog_detach(linux_uapi::bpf_attach_type_BPF_CGROUP_INET_EGRESS, root_cgroup.as_fd())
            .expect("Failed to detach program");
    }
}
