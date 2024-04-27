// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package main

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"go.fuchsia.dev/fuchsia/tools/emulator"
	"go.fuchsia.dev/fuchsia/tools/emulator/emulatortest"
	fvdpb "go.fuchsia.dev/fuchsia/tools/virtual_device/proto"
)

var cmdline = []string{"kernel.halt-on-panic=true", "kernel.bypass-debuglog=true"}

func execDir(t *testing.T) (string, error) {
	ex, err := os.Executable()
	return filepath.Dir(ex), err
}

func TestQemuWithUFSDisk(t *testing.T) {
	exDir, err := execDir(t)
	if err != nil {
		t.Fatalf("execDir() err: %s", err)
	}
	distro := emulatortest.UnpackFrom(t, filepath.Join(exDir, "test_data"), emulator.DistributionParams{
		Emulator: emulator.Qemu,
	})
	arch := distro.TargetCPU()
	device := emulator.DefaultVirtualDevice(string(arch))
	device.KernelArgs = append(device.KernelArgs, cmdline...)

	// Add UFS disk as a boot drive.
	device.Hw.Drives = append(device.Hw.Drives, &fvdpb.Drive{
		Id:         "fuchsia-ufs",
		Image:      filepath.Join(exDir, "../fuchsia.zbi"),
		IsFilename: true,
		Device:     &fvdpb.Device{Model: "ufs-storage"},
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	emu := distro.CreateContext(ctx, device)
	emu.Start()

	// This message indicates that the ufs disk was detected.
	emu.WaitForLogMessage("[driver, Ufs] INFO: Bind Success")

	// Check that the ufs disk is listed by fuchsia.
	emu.RunCommand("lsblk")
	emu.WaitForLogMessage("/00:02.0/00_02_0/ufs/scsi-disk-0-0/block")
}

func TestQemuWithUFSDiskAndRunBlktest(t *testing.T) {
	exDir, err := execDir(t)
	if err != nil {
		t.Fatalf("execDir() err: %s", err)
	}
	distro := emulatortest.UnpackFrom(t, filepath.Join(exDir, "test_data"), emulator.DistributionParams{
		Emulator: emulator.Qemu,
	})
	arch := distro.TargetCPU()
	device := emulator.DefaultVirtualDevice(string(arch))
	device.KernelArgs = append(device.KernelArgs, cmdline...)

	// Add UFS disk as a boot drive.
	device.Hw.Drives = append(device.Hw.Drives, &fvdpb.Drive{
		Id:         "fuchsia-ufs",
		Image:      filepath.Join(exDir, "../fuchsia.zbi"),
		IsFilename: true,
		Device:     &fvdpb.Device{Model: "ufs-storage"},
	})

	// Create a new empty disk to blktest to.
	f, err := os.CreateTemp(t.TempDir(), "blktest-disk")
	if err != nil {
		t.Fatal(err)
	}

	// Make it 10MB.
	if err := f.Truncate(10 * 1024 * 1024); err != nil {
		t.Fatal(err)
	}

	if err := f.Close(); err != nil {
		t.Fatal(err)
	}

	// Add UFS disk drive.
	device.Hw.Drives = append(device.Hw.Drives, &fvdpb.Drive{
		Id:         "test-ufs",
		Image:      f.Name(),
		IsFilename: true,
		Device:     &fvdpb.Device{Model: "ufs-storage"},
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	emu := distro.CreateContext(ctx, device)
	emu.Start()

	// This message indicates that the ufs disk was detected.
	emu.WaitForLogMessage("[driver, Ufs] INFO: Bind Success")

	// Check that the emulated disk is there.
	emu.RunCommand("lsblk")
	emu.WaitForLogMessage("/00:02.0/00_02_0/ufs/scsi-disk-0-0/block")
	emu.RunCommand("lsblk")
	emu.WaitForLogMessage("/00:03.0/00_03_0/ufs/scsi-disk-0-0/block")

	// Run blktest
	emu.RunCommand("blktest -d /dev/sys/platform/pt/PCI0/bus/00:03.0/00_03_0/ufs/scsi-disk-0-0/block")
	emu.WaitForLogMessage("[  PASSED  ]")
}
