// Copyright 2025 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/sync/condvar.h>

#include <acpica/acpi.h>
#include <kernel/semaphore.h>

// Semaphore implementation using condvar + mutex.
struct AcpiSemaphore {
 public:
  explicit AcpiSemaphore(uint32_t initial_count) : count_(initial_count) {}

  void Wait(uint32_t units) {
    mutex_.lock().Acquire();
    while (count_ < units) {
      condition_.Wait(&mutex_.lock());
    }
    mutex_.lock().Release();
  }

  ACPI_STATUS WaitWithDeadline(uint32_t units, zx_instant_mono_t deadline) {
    mutex_.lock().Acquire();
    zx_status_t result = ZX_OK;
    ZX_ASSERT(mutex_.lock().IsHeld());
    while (result != ZX_ERR_TIMED_OUT && count_ < units && current_mono_time() < deadline) {
      result = condition_.Wait(&mutex_.lock(), Deadline(deadline, TimerSlack::none()));
    }
    if (result == ZX_ERR_TIMED_OUT) {
      mutex_.lock().Release();
      return AE_TIME;
    }
    count_ -= units;
    mutex_.lock().Release();
    return AE_OK;
  }

  void Signal(uint32_t units) {
    mutex_.lock().Acquire();
    count_ += units;
    if (units == 1) {
      condition_.Signal();
    } else {
      condition_.Broadcast();
    }
    mutex_.lock().Release();
  }

 private:
  sync::CondVar condition_;
  DECLARE_MUTEX(AcpiSemaphore) mutex_;
  uint32_t count_ __TA_GUARDED(mutex_);
};

/**
 * @brief Create a semaphore.
 *
 * @param MaxUnits The maximum number of units this semaphore will be required
 *        to accept
 * @param InitialUnits The initial number of units to be assigned to the
 *        semaphore.
 * @param OutHandle A pointer to a locaton where a handle to the semaphore is
 *        to be returned.
 *
 * @return AE_OK The semaphore was successfully created.
 * @return AE_BAD_PARAMETER The InitialUnits is invalid or the OutHandle
 *         pointer is NULL.
 * @return AE_NO_MEMORY Insufficient memory to create the semaphore.
 */
ACPI_STATUS AcpiOsCreateSemaphore(UINT32 MaxUnits, UINT32 InitialUnits, ACPI_SEMAPHORE* OutHandle) {
  fbl::AllocChecker ac;
  AcpiSemaphore* sem = new (&ac) AcpiSemaphore(InitialUnits);
  if (!ac.check()) {
    return AE_NO_MEMORY;
  }
  *OutHandle = sem;
  return AE_OK;
}

/**
 * @brief Delete a semaphore.
 *
 * @param Handle A handle to a semaphore objected that was returned by a
 *        previous call to AcpiOsCreateSemaphore.
 *
 * @return AE_OK The semaphore was successfully deleted.
 */
ACPI_STATUS AcpiOsDeleteSemaphore(ACPI_SEMAPHORE Handle) {
  delete Handle;
  return AE_OK;
}

/**
 * @brief Wait for units from a semaphore.
 *
 * @param Handle A handle to a semaphore objected that was returned by a
 *        previous call to AcpiOsCreateSemaphore.
 * @param Units The number of units the caller is requesting.
 * @param Timeout How long the caller is willing to wait for the requested
 *        units, in milliseconds.  A value of -1 indicates that the caller
 *        is willing to wait forever. Timeout may be 0.
 *
 * @return AE_OK The requested units were successfully received.
 * @return AE_BAD_PARAMETER The Handle is invalid.
 * @return AE_TIME The units could not be acquired within the specified time.
 */
ACPI_STATUS AcpiOsWaitSemaphore(ACPI_SEMAPHORE Handle, UINT32 Units, UINT16 Timeout) {
  if (Timeout == UINT16_MAX) {
    Handle->Wait(Units);
    return AE_OK;
  }

  return Handle->WaitWithDeadline(Units, current_mono_time() + ZX_MSEC(Timeout));
}

/**
 * @brief Send units to a semaphore.
 *
 * @param Handle A handle to a semaphore objected that was returned by a
 *        previous call to AcpiOsCreateSemaphore.
 * @param Units The number of units to send to the semaphore.
 *
 * @return AE_OK The semaphore was successfully signaled.
 * @return AE_BAD_PARAMETER The Handle is invalid.
 */
ACPI_STATUS AcpiOsSignalSemaphore(ACPI_SEMAPHORE Handle, UINT32 Units) {
  if (Units == 1) {
    ((Semaphore*)Handle)->Post();
    return AE_OK;
  }
  return AE_NOT_IMPLEMENTED;
}
