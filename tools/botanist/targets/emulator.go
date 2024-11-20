// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"debug/pe"
	"encoding/hex"
	"fmt"
	"io"
	"math/rand"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"go.fuchsia.dev/fuchsia/tools/bootserver"
	"go.fuchsia.dev/fuchsia/tools/botanist"
	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"go.fuchsia.dev/fuchsia/tools/qemu"
	testrunnerconstants "go.fuchsia.dev/fuchsia/tools/testing/testrunner/constants"

	"github.com/creack/pty"
)

const (
	// DefaultEmulatorNodename is the default nodename given to an emulator target.
	DefaultEmulatorNodename = "botanist-target-emu"

	// DefaultInterfaceName is the name given to the emulated tap interface.
	defaultInterfaceName = "qemu"

	// The size in bytes of minimimum desired size for the storage-full image.
	// The image should be large enough to hold all downloaded test packages
	// for a given test shard.
	//
	// No host-side disk blocks are allocated on extension (by use of the `fvm`
	// host tool), so the operation is cheap regardless of the size we extend to.
	storageFullMinSize int64 = 17179869184 // 16 GiB

	// Minimum number of bytes of entropy bits required for the kernel's PRNG.
	minEntropyBytes uint = 32 // 256 bits

	// The experiment level at which to enable `ffx emu`.
	ffxEmuExperimentLevel = 1
)

type Target string

const (
	TargetARM64   Target = "arm64"
	TargetRISCV64 Target = "riscv64"
	TargetX64     Target = "x64"
)

type BlockDevice struct {
	// ID is the block device identifier.
	ID string

	// File is the disk image file backing the device.
	File string
}

// EmulatorConfig gives the common configuration for supported emulators.
type EmulatorConfig struct {
	// Path is a path to a directory that contains the emulator executable.
	Path string `json:"path"`

	// Target is the target to emulate.
	Target Target `json:"target"`

	// CPU is the number of processors to emulate.
	CPU int `json:"cpu"`

	// Memory is the amount of memory (in MB) to provide.
	Memory int `json:"memory"`

	// VirtualDeviceSpec is the name of the virtual device spec to pass to
	// `ffx emu start --device`. This will only be used if UseProductBundle
	// is set to true. If empty, ffx emu will use the default recommended spec
	// or the first spec in its device list.
	VirtualDeviceSpec string `json:"virtual_device"`

	// UseProductBundle specifies whether to call `ffx emu` directly with the
	// product bundle instead of a custom config. If true, the VirtualDeviceSpec
	// will be used instead of the CPU and Memory.
	UseProductBundle bool `json:"use_product_bundle"`

	// KVM specifies whether to enable hardware virtualization acceleration.
	KVM bool `json:"kvm"`

	// Serial gives whether to create a 'serial device' for the emulator instance.
	// This option should be used judiciously, as it can slow the process down.
	Serial bool `json:"serial"`

	// Logfile saves emulator standard output to a file if set.
	Logfile string `json:"logfile"`

	// EDK2Dir is a path to a directory of EDK II (UEFI) prebuilts.
	EDK2Dir string `json:"edk2_dir"`

	// Path to the fvm host tool.
	FVMTool string `json:"fvm_tool"`

	// Path to the zbi host tool.
	ZBITool string `json:"zbi_tool"`
}

// emulator is the base emulator target abstraction, wrapped by QEMU, AEMU, and
// crosvm targets.
type emulator struct {
	*genericFuchsiaTarget
	binary  string
	builder emulatorCommandBuilder
	c       chan error
	config  EmulatorConfig
	mac     [6]byte
	opts    Options
	process *os.Process
	ptm     *os.File
	serial  io.ReadWriteCloser
}

var _ FuchsiaTarget = (*emulator)(nil)

// emulatorCommandBuilder defines the common set of functions used to build up
// an emulator command-line.
type emulatorCommandBuilder interface {
	SetBinary(string)
	SetKernel(string)
	SetInitrd(string)
	SetUEFIVolumes(string, string, string)
	SetTarget(target Target, kvm bool)
	SetMemory(int)
	SetCPUCount(int)
	AddBlockDevice(BlockDevice)
	AddSerial()
	AddTapNetwork(mac string, interfaceName string)
	AddKernelArg(string)
	BuildFFXConfig() (*qemu.Config, error)
	BuildInvocation() ([]string, error)
}

// newEmulator returns a new emulator target with a given configuration.
func newEmulator(ctx context.Context, binary string, config EmulatorConfig, opts Options, builder emulatorCommandBuilder) (*emulator, error) {
	t := &emulator{
		binary:  binary,
		builder: builder,
		c:       make(chan error),
		config:  config,
		opts:    opts,
	}
	r := rand.New(rand.NewSource(time.Now().UnixNano()))
	if _, err := r.Read(t.mac[:]); err != nil {
		return nil, fmt.Errorf("failed to generate random MAC: %w", err)
	}
	// Ensure that the generated MAC address is unicast
	// https://en.wikipedia.org/wiki/MAC_address#Unicast_vs._multicast_(I/G_bit)
	t.mac[0] &= ^uint8(0x01)

	if config.Serial {
		// We can run the emulator 'in a terminal' by creating a pseudoterminal
		// slave and attaching it as the process' std(in|out|err) streams.
		// Running it in a terminal - and redirecting serial to stdio - allows
		// us to use the associated pseudoterminal master as the 'serial device'
		// for the instance.
		var err error
		// TODO(joshuaseaton): Figure out how to manage ownership so that this may
		// be closed.
		t.ptm, t.serial, err = pty.Open()
		if err != nil {
			return nil, fmt.Errorf("failed to create ptm/pts pair: %w", err)
		}
	}
	base, err := newGenericFuchsia(ctx, DefaultEmulatorNodename, "", []string{opts.SSHKey}, t.serial)
	if err != nil {
		return nil, err
	}
	t.genericFuchsiaTarget = base
	return t, nil
}

// Nodename returns the name of the target node.
func (t *emulator) Nodename() string { return DefaultEmulatorNodename }

// Serial returns the serial device associated with the target for serial i/o.
func (t *emulator) Serial() io.ReadWriteCloser {
	return t.serial
}

// SSHKey returns the private SSH key path associated with a previously embedded authorized key.
func (t *emulator) SSHKey() string {
	return t.opts.SSHKey
}

// SSHClient creates and returns an SSH client connected to the emulator target.
func (t *emulator) SSHClient() (*sshutil.Client, error) {
	addr, err := t.IPv6()
	if err != nil {
		return nil, err
	}
	return t.sshClient(addr, "qemu")
}

// Start starts the emulator target.
// TODO(https://fxbug.dev/42177999): Add logic to use PB with ffx emu
func (t *emulator) Start(ctx context.Context, images []bootserver.Image, args []string, pbPath string, isBootTest bool) (err error) {
	if t.process != nil {
		return fmt.Errorf("a process has already been started with PID %d", t.process.Pid)
	}
	cmdLine := t.builder
	cmdLine.SetTarget(t.config.Target, t.config.KVM)

	if t.config.Path == "" {
		return fmt.Errorf("directory must be set")
	}
	bin := filepath.Join(t.config.Path, t.binary)
	absBin, err := normalizeFile(bin)
	if err != nil {
		return fmt.Errorf("could not find %s binary %q: %w", t.binary, bin, err)
	}
	cmdLine.SetBinary(absBin)

	if pbPath == "" {
		return fmt.Errorf("missing product bundle")
	}

	// If a QEMU kernel was specified, use that; else, a UEFI disk image (which does not
	// require a QEMU kernel) must be specified in the product bundle to use.
	var qemuKernel, efiDisk *bootserver.Image
	// `ffx product get-image-path` prints an error message if it can't find the
	// requested image in the product bundle, which is ok. Since the error message
	// is confusing, we'll discard the output and only return an error if a
	// required image is missing.
	origStdout := t.ffx.Stdout()
	origStderr := t.ffx.Stderr()
	resetStdoutStderr := func() {
		t.ffx.SetStdoutStderr(origStdout, origStderr)
	}
	t.ffx.SetStdoutStderr(io.Discard, io.Discard)
	qemuKernel, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "qemu-kernel", "")
	if err != nil {
		return err
	}
	if qemuKernel == nil {
		if !isBootTest {
			return fmt.Errorf("failed to find qemu kernel from product bundle")
		}
		efiDisk, err = t.ffx.GetImageFromPB(ctx, pbPath, "", "", "firmware_fat")
		if err != nil {
			return err
		}
		if efiDisk == nil {
			return fmt.Errorf("failed to find either qemu kernel or efi disk from product bundle")
		}
	}

	var zbi *bootserver.Image
	zbi, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "zbi", "")
	if err != nil {
		return err
	}
	// The zbi image may not exist as part of the
	// product bundle for a boot test, which is ok.
	if zbi == nil && !isBootTest {
		return fmt.Errorf("failed to find zbi from product bundle")
	}

	// The QEMU command needs to be invoked within an empty directory, as QEMU
	// will attempt to pick up files from its working directory, one notable
	// culprit being multiboot.bin. This can result in strange behavior.
	workdir, err := os.MkdirTemp("", "emu-working-dir")
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			os.RemoveAll(workdir)
		}
	}()

	var fvmImage *bootserver.Image
	var fxfsImage *bootserver.Image
	fvmImage, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fvm", "")
	if err != nil {
		return err
	}
	fxfsImage, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fxfs", "")
	if err != nil {
		return err
	}
	resetStdoutStderr()

	if err := copyImagesToDir(ctx, workdir, false, qemuKernel, zbi, efiDisk, fvmImage, fxfsImage); err != nil {
		return err
	}

	if zbi != nil && t.config.ZBITool != "" {
		if err := embedZBIWithKey(ctx, zbi, t.config.ZBITool, t.opts.AuthorizedKey); err != nil {
			return fmt.Errorf("failed to embed zbi with key: %w", err)
		}
	}

	// Now that the images hav successfully been copied to the working
	// directory, Path points to their path on disk.
	if qemuKernel != nil {
		cmdLine.SetKernel(qemuKernel.Path)
	}
	if zbi != nil {
		cmdLine.SetInitrd(zbi.Path)
	}

	// Checks whether the reader represents a PE (Portable Executable) file, the
	// format of UEFI applications.
	isPE := func(r io.ReaderAt) bool {
		_, err := pe.NewFile(r)
		return err == nil
	}
	// If a UEFI filesystem/disk image was specified, or if the provided QEMU
	// kernel is a UEFI application, ensure that the emulator boots through UEFI.
	if efiDisk != nil || (qemuKernel != nil && isPE(qemuKernel.Reader)) {
		edk2Dir := filepath.Join(t.config.EDK2Dir, "qemu-"+string(t.config.Target))
		var code, data string
		switch t.config.Target {
		case TargetX64:
			code = filepath.Join(edk2Dir, "OVMF_CODE.fd")
			data = filepath.Join(edk2Dir, "OVMF_VARS.fd")
		case TargetARM64:
			code = filepath.Join(edk2Dir, "QEMU_EFI.fd")
			data = filepath.Join(edk2Dir, "QEMU_VARS.fd")
		}
		code, err = normalizeFile(code)
		if err != nil {
			return err
		}
		data, err = normalizeFile(data)
		if err != nil {
			return err
		}
		diskPath := ""
		if efiDisk != nil {
			diskPath = efiDisk.Path
		}
		cmdLine.SetUEFIVolumes(code, data, diskPath)
	}

	if fxfsImage != nil {
		if err := extendImage(ctx, fxfsImage, storageFullMinSize); err != nil {
			return fmt.Errorf("%s to %d bytes: %w", constants.FailedToExtendBlkMsg, storageFullMinSize, err)
		}
		cmdLine.AddBlockDevice(BlockDevice{
			ID:   "maindisk",
			File: fxfsImage.Path,
		})
	} else if fvmImage != nil {
		if t.config.FVMTool != "" {
			if err := extendFvmImage(ctx, fvmImage, t.config.FVMTool, storageFullMinSize); err != nil {
				return fmt.Errorf("%s to %d bytes: %w", constants.FailedToExtendFVMMsg, storageFullMinSize, err)
			}
		}
		cmdLine.AddBlockDevice(BlockDevice{
			ID:   "maindisk",
			File: fvmImage.Path,
		})
	}

	cmdLine.AddTapNetwork(net.HardwareAddr(t.mac[:]).String(), defaultInterfaceName)
	cmdLine.AddSerial()

	// Manually set nodename, since MAC is randomly generated.
	cmdLine.AddKernelArg("zircon.nodename=" + t.nodename)
	// The system will halt on a kernel panic instead of rebooting.
	cmdLine.AddKernelArg("kernel.halt-on-panic=true")
	// Disable kernel lockup detector in emulated environments to prevent false alarms from
	// potentially oversubscribed hosts.
	cmdLine.AddKernelArg("kernel.lockup-detector.critical-section-threshold-ms=0")
	cmdLine.AddKernelArg("kernel.lockup-detector.critical-section-fatal-threshold-ms=0")
	cmdLine.AddKernelArg("kernel.lockup-detector.heartbeat-period-ms=0")
	cmdLine.AddKernelArg("kernel.lockup-detector.heartbeat-age-threshold-ms=0")
	cmdLine.AddKernelArg("kernel.lockup-detector.heartbeat-age-fatal-threshold-ms=0")

	// Add entropy to simulate bootloader entropy.
	entropy := make([]byte, minEntropyBytes)
	if _, err := rand.Read(entropy); err == nil {
		cmdLine.AddKernelArg("kernel.entropy-mixin=" + hex.EncodeToString(entropy))
	}
	// Do not print colors.
	cmdLine.AddKernelArg("TERM=dumb")
	if t.config.Target == TargetX64 {
		// Necessary to redirect to stdout.
		cmdLine.AddKernelArg("kernel.serial=legacy")
	}
	for _, arg := range args {
		cmdLine.AddKernelArg(arg)
	}

	cmdLine.SetCPUCount(t.config.CPU)
	cmdLine.SetMemory(t.config.Memory)

	var cmd *exec.Cmd
	if t.UseFFXExperimental(ffxEmuExperimentLevel) {
		ffxConfig, err := cmdLine.BuildFFXConfig()
		if err != nil {
			return err
		}

		ffxConfigFile := filepath.Join(os.Getenv(testrunnerconstants.TestOutDirEnvKey), "ffx_emu_config.json")
		absFFXConfigFile, err := filepath.Abs(ffxConfigFile)
		if err != nil {
			return err
		}
		if err := jsonutil.WriteToFile(absFFXConfigFile, ffxConfig); err != nil {
			return err
		}
		cwd, err := os.Getwd()
		if err != nil {
			return err
		}
		edk2Dir := filepath.Join(t.config.EDK2Dir, "qemu-"+string(t.config.Target))
		var code string
		switch t.config.Target {
		case "x64":
			code = filepath.Join(edk2Dir, "OVMF_CODE.fd")
		case "arm64":
			code = filepath.Join(edk2Dir, "QEMU_EFI.fd")
		}
		tools := ffxutil.EmuTools{
			Emulator: absBin,
			FVM:      t.config.FVMTool,
			ZBI:      t.config.ZBITool,
			UEFI:     code,
		}
		startArgs := ffxutil.EmuStartArgs{
			Engine: strings.ToLower(os.Getenv("FUCHSIA_DEVICE_TYPE")),
		}
		if t.config.UseProductBundle {
			startArgs.ProductBundle = filepath.Join(cwd, pbPath)
			startArgs.KernelArgs = ffxConfig.KernelArgs
			startArgs.Device = t.config.VirtualDeviceSpec
			if t.config.KVM {
				startArgs.Accel = "hyper"
			} else {
				startArgs.Accel = "none"
			}
		} else {
			startArgs.Config = absFFXConfigFile
		}

		cmd, err = t.ffx.EmuStartConsole(ctx, cwd, DefaultEmulatorNodename, tools, startArgs)
		if err != nil {
			return err
		}
	} else {
		invocation, err := cmdLine.BuildInvocation()
		if err != nil {
			return err
		}
		cmd = exec.Command(invocation[0], invocation[1:]...)
	}

	cmd.Dir = workdir
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, t.binary)
	// Since serial is already printed to stdout, we can copy it to the
	// serial logfile as well.
	var serialLog *os.File
	closeSerialLog := func() {
		if serialLog == nil {
			return
		}
		if err := serialLog.Close(); err != nil {
			logger.Debugf(ctx, "failed to close %s", serialLog.Name())
		}
	}
	if t.config.Logfile != "" {
		logfile, err := filepath.Abs(t.config.Logfile)
		if err != nil {
			return fmt.Errorf("cannot get absolute path for %q: %w", t.config.Logfile, err)
		}
		if err := os.MkdirAll(filepath.Dir(logfile), os.ModePerm); err != nil {
			return fmt.Errorf("failed to make parent dirs of %q: %w", logfile, err)
		}
		serialLog, err := os.Create(logfile)
		if err != nil {
			return fmt.Errorf("failed to create %s", logfile)
		}
		if err := os.Setenv(constants.SerialLogEnvKey, logfile); err != nil {
			logger.Debugf(ctx, "failed to set %s to %s", constants.SerialLogEnvKey, logfile)
		}
		serialWriter := botanist.NewLineWriter(botanist.NewTimestampWriter(serialLog), "")
		stdout = io.MultiWriter(stdout, serialWriter)
		stderr = io.MultiWriter(stderr, serialWriter)
	}
	if t.ptm != nil {
		cmd.Stdin = t.ptm
		cmd.Stdout = io.MultiWriter(t.ptm, stdout)
		cmd.Stderr = io.MultiWriter(t.ptm, stderr)
		cmd.SysProcAttr = &syscall.SysProcAttr{
			Setctty: true,
			Setsid:  true,
		}
	} else {
		cmd.Stdout = stdout
		cmd.Stderr = stderr
		cmd.SysProcAttr = &syscall.SysProcAttr{
			// Set a process group ID so we can kill the entire group, meaning
			// the process and any of its children.
			Setpgid: true,
		}
	}
	logger.Debugf(ctx, "%s invocation:\n%s", t.binary, strings.Join(append([]string{cmd.Path}, cmd.Args...), " "))

	if err := cmd.Start(); err != nil {
		flush()
		closeSerialLog()
		return fmt.Errorf("failed to start: %w", err)
	}
	t.process = cmd.Process

	go func() {
		err := cmd.Wait()
		flush()
		closeSerialLog()
		if err != nil {
			err = fmt.Errorf("%s invocation error: %w", t.binary, err)
		}
		t.c <- err
		os.RemoveAll(workdir)
	}()
	return nil
}

// Stop stops the emulator target.
func (t *emulator) Stop() error {
	if t.UseFFXExperimental(ffxEmuExperimentLevel) {
		return t.ffx.EmuStop(context.Background())
	}
	if t.process == nil {
		return fmt.Errorf("%s target has not yet been started", t.binary)
	}
	logger.Debugf(t.targetCtx, "Sending SIGKILL to %d", t.process.Pid)
	err := t.process.Kill()
	t.process = nil
	t.genericFuchsiaTarget.Stop()
	return err
}

// Wait waits for the QEMU target to stop.
func (t *emulator) Wait(ctx context.Context) error {
	select {
	case err := <-t.c:
		return err
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Config returns fields describing the target.
func (t *emulator) TestConfig(expectsSSH bool) (any, error) {
	return TargetInfo(t, expectsSSH, nil)
}

func normalizeFile(path string) (string, error) {
	if _, err := os.Stat(path); err != nil {
		return "", err
	}
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return absPath, nil
}

func overwriteFileWithCopy(path string) error {
	tmpfile, err := os.CreateTemp(filepath.Dir(path), "botanist")
	if err != nil {
		return err
	}
	defer tmpfile.Close()
	if err := osmisc.CopyFile(path, tmpfile.Name()); err != nil {
		return err
	}
	return os.Rename(tmpfile.Name(), path)
}

func embedZBIWithKey(ctx context.Context, zbiImage *bootserver.Image, zbiTool string, authorizedKeysFile string) error {
	absToolPath, err := filepath.Abs(zbiTool)
	if err != nil {
		return err
	}
	logger.Debugf(ctx, "embedding %s with key %s", zbiImage.Name, authorizedKeysFile)
	cmd := exec.CommandContext(ctx, absToolPath, "-o", zbiImage.Path, zbiImage.Path, "--entry", fmt.Sprintf("data/ssh/authorized_keys=%s", authorizedKeysFile))
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "zbi")
	defer flush()
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Run(); err != nil {
		return err
	}
	return nil
}

func extendFvmImage(ctx context.Context, fvmImage *bootserver.Image, fvmTool string, size int64) error {
	if fvmTool == "" {
		return nil
	}
	absToolPath, err := filepath.Abs(fvmTool)
	if err != nil {
		return err
	}
	logger.Debugf(ctx, "extending fvm.blk to %d bytes", size)
	cmd := exec.CommandContext(ctx, absToolPath, fvmImage.Path, "extend", "--length", strconv.Itoa(int(size)))
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "fvm")
	defer flush()
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Run(); err != nil {
		return err
	}
	fvmImage.Size = size
	return nil
}

func extendImage(ctx context.Context, image *bootserver.Image, size int64) error {
	if image.Size >= size {
		return nil
	}
	if err := os.Truncate(image.Path, size); err != nil {
		return err
	}
	image.Size = size
	return nil
}

func getImageByNameAndCPU(imgs []bootserver.Image, name, cpu string) *bootserver.Image {
	for _, img := range imgs {
		if img.Name == name && img.CPU == cpu {
			return &img
		}
	}
	return nil
}
