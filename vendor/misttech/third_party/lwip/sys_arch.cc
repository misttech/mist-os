// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "arch/sys_arch.h"

#include <trace.h>

#include <fbl/canary.h>
#include <kernel/mutex.h>
#include <kernel/semaphore.h>
#include <kernel/spinlock.h>
#include <kernel/thread.h>
#include <lk/init.h>

#include "lwip/debug.h"
#include "lwip/opt.h"
#include "lwip/sys.h"
#include "lwip/tcpip.h"

#define LOCAL_TRACE 0

struct sys_sem {
 public:
  sys_sem(int64_t initial_count) : sem_(initial_count) {}

  zx_status_t Wait(zx_duration_mono_t timeout) {
    return timeout == 0 ? sem_.Wait(Deadline::infinite())
                        : sem_.Wait(Deadline::after_mono(timeout));
  }

  void Post() { sem_.Post(); }

 private:
  Semaphore sem_;
};

struct sys_mutex {
  DECLARE_MUTEX(sys_mutex) mutex;
};

#define SYS_MBOX_SIZE 16

struct sys_mbox {
  fbl::Canary<fbl::magic("MBOX")> magic;

  Semaphore empty;
  Semaphore full;
  DECLARE_MUTEX(sys_mbox) lock;

  int head;
  int tail;
  int size;
  void *queue[SYS_MBOX_SIZE];
};

/*-----------------------------------------------------------------------------------*/
/* Time */

u32_t sys_now(void) { return (u32_t)(current_mono_time() / ZX_MSEC(1)); }

// u32_t sys_jiffies(void) { return static_cast<u32_t>(current_boot_time()); }

/*-----------------------------------------------------------------------------------*/
/* Init */

void sys_init(void) {}

/*-----------------------------------------------------------------------------------*/
/* Threads */

struct thread_wrapper_data {
  lwip_thread_fn function;
  void *arg;
};

static int thread_wrapper(void *arg) {
  struct thread_wrapper_data *thread_data = (struct thread_wrapper_data *)arg;

  thread_data->function(thread_data->arg);

  /* we should never get here */
  delete thread_data;
  return 0;
}

sys_thread_t sys_thread_new(const char *name, lwip_thread_fn func, void *arg, int stacksize,
                            int prio) {
  LWIP_UNUSED_ARG(stacksize);
  LWIP_UNUSED_ARG(prio);

  fbl::AllocChecker ac;
  struct thread_wrapper_data *thread_data = new (&ac) struct thread_wrapper_data();
  if (!ac.check()) {
    return nullptr;
  }
  thread_data->function = func;
  thread_data->arg = arg;

  Thread *thread = reinterpret_cast<sys_thread_t>(
      Thread::Create(name, thread_wrapper, thread_data, DEFAULT_PRIORITY));
  thread->Detach();
  thread->Resume();
  return thread;
}

/*-----------------------------------------------------------------------------------*/
/* Mutex */

err_t sys_mutex_new(struct sys_mutex **mutex) {
  struct sys_mutex *mtx;

  fbl::AllocChecker ac;
  mtx = new (&ac) sys_mutex();
  if (!ac.check()) {
    return ERR_MEM;
  }
  *mutex = mtx;
  return ERR_OK;
}

void sys_mutex_lock(struct sys_mutex **mutex) TA_ACQ((*mutex)->mutex) {
  (*mutex)->mutex.lock().Acquire();
}

void sys_mutex_unlock(struct sys_mutex **mutex) TA_REL((*mutex)->mutex) {
  (*mutex)->mutex.lock().Release();
}

void sys_mutex_free(struct sys_mutex **mutex) TA_EXCL((*mutex)->mutex) { delete *mutex; }

/*-----------------------------------------------------------------------------------*/
/* Critical section */

#if SYS_LIGHTWEIGHT_PROT

namespace {
DECLARE_SINGLETON_SPINLOCK(ArchLock);
}

sys_prot_t sys_arch_protect(void) TA_ACQ(ArchLock::Get()) {
  interrupt_saved_state_t irqstate;
  ArchLock::Get()->lock().AcquireIrqSave(irqstate);
  return irqstate;
}

void sys_arch_unprotect(sys_prot_t pval) TA_REL(ArchLock::Get()) {
  ArchLock::Get()->lock().ReleaseIrqRestore(pval);
}

#endif /* SYS_LIGHTWEIGHT_PROT */

/*-----------------------------------------------------------------------------------*/
/* Mailbox */

err_t sys_mbox_new(struct sys_mbox **mb, int size) {
  LTRACE_ENTRY;
  struct sys_mbox *mbox;
  LWIP_UNUSED_ARG(size);

  fbl::AllocChecker ac;
  mbox = new (&ac) sys_mbox();
  if (!ac.check()) {
    return ERR_MEM;
  }

  mbox->head = 0;
  mbox->tail = 0;

  SYS_STATS_INC_USED(mbox);

  *mb = mbox;
  return ERR_OK;
}

void sys_mbox_free(struct sys_mbox **mb) {
  if ((mb != nullptr) && (*mb != nullptr)) {
    struct sys_mbox *mbox = *mb;
    SYS_STATS_DEC(mbox.used);
    /*  LWIP_DEBUGF("sys_mbox_free: mbox 0x%lx\n", mbox); */
    delete mbox;
  }
}

int sys_mbox_valid(struct sys_mbox **mb) {
  return (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid();
}

void sys_mbox_post(struct sys_mbox **mb, void *msg) {
  LTRACE_ENTRY;
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  mbox->empty.Wait(Deadline::infinite());

  LWIP_DEBUGF(SYS_DEBUG, ("sys_mbox_post: mbox %p msg %p\n", (void *)mbox, (void *)msg));
  {
    Guard<Mutex> guard{&mbox->lock};
    mbox->queue[mbox->head] = msg;
    mbox->head = (mbox->head + 1) % SYS_MBOX_SIZE;
  }
  mbox->full.Post();
  LTRACE_EXIT;
}

err_t sys_mbox_trypost(struct sys_mbox **mb, void *msg) {
  LTRACE_ENTRY;
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  zx_status_t res = mbox->empty.Wait(Deadline::after_mono(ZX_USEC(1)));
  if (res == ZX_ERR_TIMED_OUT) {
    return ERR_MEM;
  }

  {
    Guard<Mutex> guard{&mbox->lock};

    mbox->queue[mbox->head] = msg;
    mbox->head = (mbox->head + 1) % SYS_MBOX_SIZE;
  }
  mbox->full.Post();
  return ERR_OK;
}

u32_t sys_arch_mbox_tryfetch(struct sys_mbox **mb, void **msg) {
  LTRACE_ENTRY;
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  zx_status_t res = mbox->full.Wait(Deadline::after_mono(ZX_USEC(1)));
  if (res == ZX_ERR_TIMED_OUT) {
    return SYS_MBOX_EMPTY;
  }

  if (mbox->head == mbox->tail) {
    return SYS_MBOX_EMPTY;
  }

  {
    Guard<Mutex> guard{&mbox->lock};

    if (msg != nullptr) {
      LWIP_DEBUGF(SYS_DEBUG, ("sys_mbox_tryfetch: mbox %p msg %p\n", (void *)mbox, *msg));
      *msg = mbox->queue[mbox->tail];
    }
    mbox->tail = (mbox->tail + 1) % SYS_MBOX_SIZE;
  }
  mbox->empty.Post();

  return 0;
}

u32_t sys_arch_mbox_fetch(struct sys_mbox **mb, void **msg, u32_t timeout) {
  struct sys_mbox *mbox;
  LWIP_ASSERT("invalid mbox", (mb != nullptr) && (*mb != nullptr) && (*mb)->magic.Valid());
  mbox = *mb;

  u32_t start = sys_now();
  zx_status_t res =
      mbox->full.Wait(timeout ? Deadline::after_mono(ZX_MSEC(timeout)) : Deadline::infinite());
  if (res != ZX_OK) {
    return SYS_ARCH_TIMEOUT;
  }

  {
    Guard<Mutex> guard{&mbox->lock};

    if (msg != nullptr) {
      LWIP_DEBUGF(SYS_DEBUG, ("sys_mbox_fetch: mbox %p msg %p\n", (void *)mbox, *msg));
      *msg = mbox->queue[mbox->tail];
    }
    mbox->tail = (mbox->tail + 1) % SYS_MBOX_SIZE;
  }
  mbox->empty.Post();

  return sys_now() - start;
}

/*-----------------------------------------------------------------------------------*/
/* Semaphore */

err_t sys_sem_new(struct sys_sem **sem, u8_t count) {
  LWIP_UNUSED_ARG(count);

  fbl::AllocChecker ac;
  *sem = new (&ac) sys_sem(count);
  if (!ac.check()) {
    return ERR_MEM;
  }
  return ERR_OK;
}

void sys_sem_free(struct sys_sem **sem) { delete *sem; }

void sys_sem_signal(struct sys_sem **s) {
  struct sys_sem *sem;
  LWIP_ASSERT("invalid sem", (s != nullptr) && (*s != nullptr));
  sem = *s;
  sem->Post();
}

u32_t sys_arch_sem_wait(struct sys_sem **s, u32_t timeout) {
  LTRACE_ENTRY;
  struct sys_sem *sem;
  LWIP_ASSERT("invalid sem", (s != nullptr) && (*s != nullptr));
  sem = *s;

  u32_t start = sys_now();
  zx_status_t res = sem->Wait(timeout);
  if (res == ZX_ERR_TIMED_OUT) {
    return SYS_ARCH_TIMEOUT;
  }

  return sys_now() - start;
}

namespace {
void tcpip_init_done(void *arg) { dprintf(INFO, "TPC/IP init done\n"); }
}  // namespace

/* run lwip init as soon as threads are running */
void lwip_init_hook(uint level) { tcpip_init(tcpip_init_done, nullptr); }

LK_INIT_HOOK(lwip, &lwip_init_hook, LK_INIT_LEVEL_PLATFORM)
