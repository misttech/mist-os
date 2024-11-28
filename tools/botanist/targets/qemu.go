// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"fmt"

	"go.fuchsia.dev/fuchsia/tools/qemu"
)

const (
	// qemuSystemPrefix is the prefix of the QEMU binary name, which is of the
	// form qemu-system-<QEMU arch suffix>.
	qemuSystemPrefix = "qemu-system"
)

// qemuTargetMapping maps the Fuchsia target name to the name recognized by QEMU.
var qemuTargetMapping = map[Target]qemu.Target{
	TargetX64:     qemu.TargetEnum.X86_64,
	TargetARM64:   qemu.TargetEnum.AArch64,
	TargetRISCV64: qemu.TargetEnum.RiscV64,
}

// QEMU is a QEMU target.
type QEMU struct {
	emulator
}

var _ FuchsiaTarget = (*QEMU)(nil)

// NewQEMU returns a new QEMU target with a given configuration.
func NewQEMU(ctx context.Context, config EmulatorConfig, opts Options) (*QEMU, error) {
	qemuTarget, ok := qemuTargetMapping[config.Target]
	if !ok {
		return nil, fmt.Errorf("invalid target %q", config.Target)
	}
	bin := fmt.Sprintf("%s-%s", qemuSystemPrefix, qemuTarget)
	return newQEMU(ctx, bin, &qemu.QEMUCommandBuilder{}, config, opts)
}

func newQEMU(ctx context.Context, binary string, builder baseQEMUCommandBuilder, config EmulatorConfig, opts Options) (*QEMU, error) {
	emu, err := newEmulator(ctx, binary, config, opts, &qemuCommandBuilder{nil, builder})
	if err != nil {
		return nil, err
	}
	return &QEMU{emulator: *emu}, nil
}

// As defined in the qemu package.
type baseQEMUCommandBuilder interface {
	SetFlag(...string)
	SetBinary(string)
	SetKernel(string)
	SetInitrd(string)
	SetUEFIVolumes(string, string, string)
	SetTarget(qemu.Target, bool)
	SetMemory(int)
	SetCPUCount(int)
	AddVirtioBlkPciDrive(qemu.Drive)
	AddRNG(qemu.RNGdev)
	AddSerial(qemu.Chardev)
	AddNetwork(qemu.Netdev)
	AddKernelArg(string)
	Build() ([]string, error)
	BuildConfig() (qemu.Config, error)
}

// qemuCommandBuilder implements the emulatorCommandBuilder interface for the
// QEMU and AEMU targets.
type qemuCommandBuilder struct {
	target *Target
	baseQEMUCommandBuilder
}

func (b *qemuCommandBuilder) SetTarget(target Target, kvm bool) {
	qemuTarget, ok := qemuTargetMapping[target]
	if !ok {
		panic(fmt.Sprintf("invalid target %q", target))
	}
	b.baseQEMUCommandBuilder.SetTarget(qemuTarget, kvm)
	b.target = &target
}

func (b *qemuCommandBuilder) AddBlockDevice(device BlockDevice) {
	drive := qemu.Drive{ID: device.ID, File: device.File}
	b.baseQEMUCommandBuilder.AddVirtioBlkPciDrive(drive)
}
func (b *qemuCommandBuilder) AddSerial() {
	chardev := qemu.Chardev{
		ID:     "char0",
		Signal: false,
	}
	b.baseQEMUCommandBuilder.AddSerial(chardev)
}
func (b *qemuCommandBuilder) AddTapNetwork(mac string, interfaceName string) {
	netdev := qemu.Netdev{
		ID:     "net0",
		Device: qemu.Device{Model: qemu.DeviceModelVirtioNetPCI},
	}
	netdev.Device.AddOption("mac", mac)
	netdev.Device.AddOption("vectors", "8")
	netdev.Tap = &qemu.NetdevTap{Name: interfaceName}
	b.baseQEMUCommandBuilder.AddNetwork(netdev)
}

func (b *qemuCommandBuilder) BuildFFXConfig() (*qemu.Config, error) {
	b.setQEMUSpecifics()
	cfg, err := b.baseQEMUCommandBuilder.BuildConfig()
	if err != nil {
		return nil, err
	}
	return &cfg, nil
}

func (b *qemuCommandBuilder) BuildInvocation() ([]string, error) {
	b.setQEMUSpecifics()
	return b.baseQEMUCommandBuilder.Build()
}

func (b *qemuCommandBuilder) setQEMUSpecifics() {
	if b.target == nil {
		panic("target not set!")
	}
	// If we're running on RISC-V, we need to add an RNG device.
	if *b.target == TargetRISCV64 {
		rngdev := qemu.RNGdev{
			ID:       "rng0",
			Device:   qemu.Device{Model: qemu.DeviceModelRNG},
			Filename: "/dev/urandom",
		}
		b.AddRNG(rngdev)
	}
	b.SetFlag("-nographic")
	b.SetFlag("-monitor", "none")
}
