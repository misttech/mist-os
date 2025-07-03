// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <stdio.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

#include <atomic>
#include <string>

#include <linux/audit.h>
#include <linux/netlink.h>

// When set to true, the audit listener will call recv without waiting and exit when the queue is
// empty.
std::atomic<bool> g_audit_listener_exiting = false;

void AuditListenerSignalHandler(int signal) { g_audit_listener_exiting = true; }

void AuditListener() {
  struct sigaction handler = {};
  handler.sa_handler = AuditListenerSignalHandler;
  if (sigaction(SIGUSR1, &handler, nullptr) < 0) {
    perror("audit_listener: sigaction");
    exit(1);
  }
  int fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_AUDIT);
  if (fd < 0) {
    perror("audit_listener: error creating socket");
    exit(1);
  }
  struct {
    nlmsghdr header;
    audit_status payload;
  } packet = {};
  packet.header.nlmsg_len = sizeof(packet);
  packet.header.nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK;
  packet.header.nlmsg_seq = 1;
  packet.header.nlmsg_type = AUDIT_SET;
  packet.payload.enabled = 1;
  packet.payload.pid = getpid();
  packet.payload.mask = AUDIT_STATUS_ENABLED | AUDIT_STATUS_PID;

  struct sockaddr_nl addr = {};
  addr.nl_family = AF_NETLINK;

  sendto(fd, &packet, sizeof(packet), 0, reinterpret_cast<sockaddr*>(&addr), sizeof(addr));

  char buffer[65536];
  ssize_t slen = 0;
  while ((slen = recv(fd, buffer, sizeof(buffer),
                      g_audit_listener_exiting.load() ? MSG_DONTWAIT : 0)) > 0 ||
         errno == EINTR) {
    if (slen < 0) {
      continue;
    }
    size_t len = slen;
    struct nlmsghdr header;
    if (len < sizeof(header)) {
      fprintf(stderr, "audit_listener: message too short\n");
      continue;
    }
    memcpy(&header, buffer, sizeof(header));
    if (header.nlmsg_type == NLMSG_ERROR) {
      struct nlmsgerr err;
      if (len < sizeof(header) + sizeof(err)) {
        fprintf(stderr, "audit_listener: error message too short\n");
        continue;
      }
      memcpy(&err, buffer + sizeof(header), sizeof(err));
      if (err.error == 0) {
        fprintf(stderr, "audit_listener: audit started\n");
      } else {
        fprintf(stderr, "audit_listener: received error %d\n", -err.error);
      }
    } else if (header.nlmsg_type == AUDIT_AVC) {
      std::string message(buffer + sizeof(header), buffer + len);
      fprintf(stderr, "%s\n", message.c_str());
    }
  }
  if (slen < 0 && !(g_audit_listener_exiting.load() && (errno == EWOULDBLOCK || errno == EAGAIN))) {
    perror("recv netlink");
  }
  fprintf(stderr, "audit_listener: stopped receiving\n");
}

int main(int argc, char** argv) {
  auto reboot_before_exit = fit::defer([] { reboot(RB_POWER_OFF); });

  pid_t audit_pid = fork();
  if (audit_pid == 0) {
    AuditListener();
    exit(1);
  }

  pid_t child_pid = fork();
  if (child_pid == -1) {
    perror("fork() failed");
    return 1;
  }

  if (child_pid == 0) {
    execv(argv[1], argv + 1);
    perror("exec failed");
    exit(1);
  }

  int wstatus;
  if (waitpid(child_pid, &wstatus, 0) == -1) {
    perror("waitpid() failed");
    return 1;
  }
  if (WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0) {
    fprintf(stderr, "TEST SUCCESS\n");
  } else {
    fprintf(stderr, "TEST FAILURE\n");
  }

  kill(audit_pid, SIGUSR1);
  waitpid(audit_pid, &wstatus, 0);

  return 0;
}
