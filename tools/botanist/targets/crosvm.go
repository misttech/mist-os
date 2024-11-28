// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"fmt"

	"go.fuchsia.dev/fuchsia/tools/qemu"
)

type Crosvm struct {
	emulator
}

func NewCrosvm(ctx context.Context, config EmulatorConfig, opts Options) (*Crosvm, error) {
	emu, err := newEmulator(ctx, "crosvm", config, opts, &crosvmCommandBuilder{})
	if err != nil {
		return nil, err
	}
	return &Crosvm{*emu}, nil
}

type crosvmCommandBuilder struct {
	binary     string
	kernel     string
	kernelArgs []string
	cmd        []string
}

func (b *crosvmCommandBuilder) SetBinary(binary string) {
	b.binary = binary
}

func (b *crosvmCommandBuilder) SetKernel(kernel string) {
	b.kernel = kernel
}

func (b *crosvmCommandBuilder) SetInitrd(zbi string) {
	b.cmd = append(b.cmd, "--initrd", zbi)
}

func (b *crosvmCommandBuilder) SetUEFIVolumes(string, string, string) {}

func (b *crosvmCommandBuilder) SetTarget(_ Target, kvm bool) {
	if !kvm {
		panic("crosvm requires KVM")
	}
}

func (b *crosvmCommandBuilder) SetMemory(mb int) {
	b.cmd = append(b.cmd, "--mem", fmt.Sprintf("size=%d", mb))
}

func (b *crosvmCommandBuilder) SetCPUCount(count int) {
	b.cmd = append(b.cmd, "--cpus", fmt.Sprintf("num-cores=%d", count))
}

// crosvm emulates a PC-style ns8250 by default.
func (b *crosvmCommandBuilder) AddSerial() {
	b.cmd = append(b.cmd, "--serial", "type=stdout,stdin,hardware=serial,earlycon")
}

func (b *crosvmCommandBuilder) AddBlockDevice(blk BlockDevice) {
	b.cmd = append(b.cmd, "--block", fmt.Sprintf("path=%s,id=%s", blk.File, blk.ID))
}

func (b *crosvmCommandBuilder) AddTapNetwork(mac string, interfaceName string) {
	b.cmd = append(b.cmd, "--net", fmt.Sprintf("tap-name=%s,mac=%s", interfaceName, mac))
}

func (b *crosvmCommandBuilder) AddKernelArg(arg string) {
	b.kernelArgs = append(b.kernelArgs, arg)
}

func (b *crosvmCommandBuilder) BuildFFXConfig() (*qemu.Config, error) {
	cmd := append([]string{"run", "--disable-sandbox"}, b.cmd...)
	return &qemu.Config{
		Args:       append(cmd, b.kernel),
		Envs:       make(map[string]string),
		Features:   []string{},
		KernelArgs: b.kernelArgs,
		Options:    []string{},
	}, nil
}

func (b *crosvmCommandBuilder) BuildInvocation() ([]string, error) {
	cmd := []string{
		b.binary,
		"run",

		// We are unlikely to have the CAP_SYS_ADMIN capability, so forgo sandboxing.
		"--disable-sandbox",
	}

	cmd = append(cmd, b.cmd...)
	for _, arg := range b.kernelArgs {
		cmd = append(cmd, "--params", arg)
	}
	cmd = append(cmd, b.kernel)
	return cmd, nil
}
