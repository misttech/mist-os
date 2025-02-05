// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"time"

	"go.fuchsia.dev/fuchsia/tools/bootserver"
	"go.fuchsia.dev/fuchsia/tools/botanist"
	"go.fuchsia.dev/fuchsia/tools/lib/iomisc"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
	"go.fuchsia.dev/fuchsia/tools/lib/serial"
	serialconstants "go.fuchsia.dev/fuchsia/tools/lib/serial/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/subprocess"
	"go.fuchsia.dev/fuchsia/tools/net/netboot"
	"go.fuchsia.dev/fuchsia/tools/net/netutil"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"go.fuchsia.dev/fuchsia/tools/net/tftp"

	"golang.org/x/crypto/ssh"
)

const (
	// Command to dump the zircon debug log over serial.
	dlogCmd = "\ndlog\n"

	// String to look for in serial log that indicates system booted. From
	// https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/kernel/top/main.cc;l=116;drc=6a0fd696cde68b7c65033da57ab911ee5db75064
	bootedLogSignature = "welcome to Zircon"

	// Idling in fastboot
	fastbootIdleSignature = "USB RESET"

	// Timeout for the overall "recover device by hard power-cycling and
	// forcing into fastboot" flow
	hardRecoveryTimeoutSecs = 60

	// Timeout to observe fastbootIdleSignature before proceeding anyway
	// after hard power cycle
	fastbootIdleWaitTimeoutSecs = 10

	// Whether we should place the device in Zedboot if idling in fastboot.
	// TODO(https://fxbug.dev/42075766): Remove once release branches no longer need this.
	mustLoadThroughZedboot = false

	// One of the possible InstallMode mode types.
	// Indicates that the target device is idling in fastboot mode.
	// TOD0(betramlalusha): Remove once NUC11 migration to flashing is complete.
	fastbootMode = "fastboot"
)

// DeviceConfig contains the static properties of a target device.
type DeviceConfig struct {
	// FastbootSernum is the fastboot serial number of the device.
	FastbootSernum string `json:"fastboot_sernum"`

	// Network is the network properties of the target.
	Network NetworkProperties `json:"network"`

	// SSHKeys are the default system keys to be used with the device.
	SSHKeys []string `json:"keys,omitempty"`

	// Serial is the path to the device file for serial i/o.
	Serial string `json:"serial,omitempty"`

	// SerialMux is the path to the device's serial multiplexer.
	SerialMux string `json:"serial_mux,omitempty"`

	// PDU is an optional reference to the power distribution unit controlling
	// power delivery to the target. This will not always be present.
	PDU *targetPDU `json:"pdu,omitempty"`

	// MaxFlashAttempts is an optional integer indicating the number of
	// attempts we will make to provision this device via flashing.  If not
	// present, we will assume its value to be 1.  It should only be set >1
	// for hardware types which have silicon bugs which make flashing
	// unreliable in a way that we cannot address with any other means.
	// Other failure modes should be resolved by fixing the source of the
	// failure, not papering over it with retries.
	MaxFlashAttempts int `json:"max_flash_attempts,omitempty"`

	// InstallMode is an optional string that tells botanist what
	// method to use to install fuchsia. This will be used to help
	// botanist distinguish between NUC11s that should be paved and
	// those that will be flashed (if the install_mode is fastboot).
	// TOD0(betramlalusha): Remove once NUC11 migration to flashing is complete.
	InstallMode string `json:"install_mode,omitempty"`
}

// NetworkProperties are the static network properties of a target.
type NetworkProperties struct {
	// Nodename is the hostname of the device that we want to boot on.
	Nodename string `json:"nodename"`

	// IPv4Addr is the IPv4 address, if statically given. If not provided, it may be
	// resolved via the netstack's mDNS server.
	IPv4Addr string `json:"ipv4"`
}

// LoadDeviceConfigs unmarshals a slice of device configs from a given file.
func LoadDeviceConfigs(path string) ([]DeviceConfig, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read device properties file %q", path)
	}

	var configs []DeviceConfig
	if err := json.Unmarshal(data, &configs); err != nil {
		return nil, fmt.Errorf("failed to unmarshal configs: %w", err)
	}
	return configs, nil
}

// Device represents a physical Fuchsia device.
type Device struct {
	*genericFuchsiaTarget
	config   DeviceConfig
	opts     Options
	signers  []ssh.Signer
	serial   io.ReadWriteCloser
	tftp     tftp.Client
	stopping uint32
}

var _ FuchsiaTarget = (*Device)(nil)

// NewDevice returns a new device target with a given configuration.
func NewDevice(ctx context.Context, config DeviceConfig, opts Options) (*Device, error) {
	// If an SSH key is specified in the options, prepend it the configs list so that it
	// corresponds to the authorized key that would be paved.
	if opts.SSHKey != "" {
		config.SSHKeys = append([]string{opts.SSHKey}, config.SSHKeys...)
	}
	signers, err := parseOutSigners(config.SSHKeys)
	if err != nil {
		return nil, fmt.Errorf("could not parse out signers from private keys: %w", err)
	}
	var s io.ReadWriteCloser
	if config.SerialMux != "" {
		if config.FastbootSernum == "" {
			s, err = serial.NewSocket(ctx, config.SerialMux)
			if err != nil {
				return nil, fmt.Errorf("unable to open: %s: %w", config.SerialMux, err)
			}
		} else {
			// We don't want to wait for the console to be ready if the device
			// is idling in Fastboot, as Fastboot does not have an interactive
			// serial console.
			s, err = serial.NewSocketWithIOTimeout(ctx, config.SerialMux, 2*time.Minute, false)
			if err != nil {
				return nil, fmt.Errorf("unable to open: %s: %w", config.SerialMux, err)
			}
		}
		// After we've made a serial connection to determine the device is ready,
		// we should close this socket since it is no longer needed. New interactions
		// with the device over serial will create new connections with the serial mux.
		s.Close()
		s = nil
	} else if config.Serial != "" {
		s, err = serial.Open(config.Serial)
		if err != nil {
			return nil, fmt.Errorf("unable to open %s: %w", config.Serial, err)
		}
	}
	base, err := newGenericFuchsia(ctx, config.Network.Nodename, config.SerialMux, config.SSHKeys, s)
	if err != nil {
		return nil, err
	}
	return &Device{
		genericFuchsiaTarget: base,
		config:               config,
		opts:                 opts,
		signers:              signers,
		serial:               s,
	}, nil
}

// Tftp returns a tftp client interface for the device.
func (t *Device) Tftp() tftp.Client {
	return t.tftp
}

// Nodename returns the name of the node.
func (t *Device) Nodename() string {
	return t.config.Network.Nodename
}

// Serial returns the serial device associated with the target for serial i/o.
func (t *Device) Serial() io.ReadWriteCloser {
	return t.serial
}

// IPv4 returns the IPv4 address of the device.
func (t *Device) IPv4() (net.IP, error) {
	return net.ParseIP(t.config.Network.IPv4Addr), nil
}

// IPv6 returns the IPv6 of the device.
// TODO(rudymathu): Re-enable mDNS resolution of IPv6 once it is no longer
// flaky on hardware.
func (t *Device) IPv6() (*net.IPAddr, error) {
	return nil, nil
}

// SSHKey returns the private SSH key path associated with the authorized key to be paved.
func (t *Device) SSHKey() string {
	return t.config.SSHKeys[0]
}

// SSHClient returns an SSH client connected to the device.
func (t *Device) SSHClient() (*sshutil.Client, error) {
	addr, err := t.IPv4()
	if err != nil {
		return nil, err
	}
	return t.sshClient(&net.IPAddr{IP: addr}, "device")
}

func (t *Device) mustLoadThroughZedboot() bool {
	return mustLoadThroughZedboot || (t.config.FastbootSernum == "" && t.config.InstallMode != fastbootMode)
}

// Start starts the device target.
func (t *Device) Start(ctx context.Context, images []bootserver.Image, args []string, pbPath string, isBootTest bool) error {
	serialSocketPath := t.SerialSocketPath()

	// Set up log listener and dump kernel output to stdout.
	l, err := netboot.NewLogListener(t.Nodename())
	if err != nil {
		return fmt.Errorf("cannot listen: %w", err)
	}
	stdout, _, flush := botanist.NewStdioWriters(ctx, "device")
	defer flush()
	go func() {
		defer l.Close()
		for atomic.LoadUint32(&t.stopping) == 0 {
			data, err := l.Listen()
			if err != nil {
				continue
			}
			if _, err := stdout.Write([]byte(data)); err != nil {
				logger.Warningf(ctx, "failed to write log to stdout: %s, data: %s", err, data)
			}
		}
	}()

	// Get authorized keys from the ssh signers.
	// We cannot have signers in netboot because there is no notion
	// of a hardware backed key when you are not booting from disk
	var authorizedKeys []byte
	if !t.opts.Netboot {
		if len(t.signers) > 0 {
			for _, s := range t.signers {
				authorizedKey := ssh.MarshalAuthorizedKey(s.PublicKey())
				authorizedKeys = append(authorizedKeys, authorizedKey...)
			}
		}
	}

	bootedLogChan := make(chan error)
	if serialSocketPath != "" {
		// Start searching for the string before we reboot, otherwise we can miss it.
		go func() {
			logger.Debugf(ctx, "watching serial for string that indicates device has booted: %q", bootedLogSignature)
			socket, err := net.Dial("unix", serialSocketPath)
			if err != nil {
				bootedLogChan <- fmt.Errorf("%s: %w", serialconstants.FailedToOpenSerialSocketMsg, err)
				return
			}
			defer socket.Close()
			_, err = iomisc.ReadUntilMatchString(ctx, socket, bootedLogSignature)
			bootedLogChan <- err
		}()
	}

	// TODO(https://fxbug.dev/355507826): Remove once ffx supports flashing Kola/Sorrel.
	deviceType := os.Getenv("FUCHSIA_DEVICE_TYPE")
	useFastbootFlashing := t.config.FastbootSernum != "" && (deviceType == "Kola" || deviceType == "Sorrel")

	// Boot Fuchsia.
	if t.config.FastbootSernum != "" || t.config.InstallMode == fastbootMode {
		// Copy images locally, as fastboot does not support flashing
		// from a remote location.
		// TODO(rudymathu): Transport these images via isolate for improved caching performance.
		wd, err := os.Getwd()
		if err != nil {
			return err
		}
		var imgs []bootserver.Image
		for _, img := range images {
			img := img
			if t.neededForFlashing(img) {
				imgs = append(imgs, img)
			}
		}
		{
			imgPtrs := make([]*bootserver.Image, len(images))
			for i := range imgs {
				imgPtrs = append(imgPtrs, &imgs[i])
			}
			if err := copyImagesToDir(ctx, wd, true, imgPtrs...); err != nil {
				return err
			}
		}

		if t.mustLoadThroughZedboot() {
			if err := t.bootZedboot(ctx, imgs); err != nil {
				return err
			}
		} else if useFastbootFlashing {
			if err := t.fastbootFlash(ctx, pbPath, imgs); err != nil {
				return err
			}
		} else {
			maxAllowedAttempts := 1
			if t.config.MaxFlashAttempts > maxAllowedAttempts {
				maxAllowedAttempts = t.config.MaxFlashAttempts
			}
			var err error
			tcpFlash := false
			target := t.config.FastbootSernum
			if target == "" {
				ipv6, err := t.genericFuchsiaTarget.IPv6()
				if err != nil {
					return err
				}

				target = ipv6.String()
				tcpFlash = true
			}
			for attempt := 1; attempt <= maxAllowedAttempts; attempt++ {
				logger.Debugf(ctx, "Starting flash attempt %d/%d", attempt, maxAllowedAttempts)
				if t.opts.Netboot {
					if err = t.ffx.BootloaderBoot(ctx, target, pbPath); err == nil {
						// If successful, early exit.
						break
					}
				} else {
					if err = t.flash(ctx, pbPath, target, tcpFlash); err == nil {
						// If successful, early exit.
						break
					}
				}
				if attempt == maxAllowedAttempts {
					logger.Errorf(ctx, "Flashing attempt %d/%d failed: %s.", attempt, maxAllowedAttempts, err)
					return err
				} else {
					// If not successful, and we have
					// remaining attempts, try hard
					// power-cycling the target to recover.
					logger.Warningf(ctx, "Flashing attempt %d/%d failed: %s.  Attempting hard power cycle.", attempt, maxAllowedAttempts, err)
					err = t.hardPowerCycleAndPlaceInFastboot(ctx)
					if err != nil {
						errWrapped := fmt.Errorf("while hard power cycling and placing device back in fastboot: %w", err)
						logger.Errorf(ctx, "%s", errWrapped)
						return errWrapped
					}
				}
			}
		}
	}
	if t.mustLoadThroughZedboot() {
		// Initialize the tftp client if:
		// 1. It is currently uninitialized.
		// 2. The device has been placed in Zedboot.
		if t.tftp == nil {
			// Discover the node on the network and initialize a tftp client to
			// talk to it.
			addr, err := netutil.GetNodeAddress(ctx, t.Nodename())
			if err != nil {
				return err
			}
			tftpClient, err := tftp.NewClient(&net.UDPAddr{
				IP:   addr.IP,
				Port: tftp.ClientPort,
				Zone: addr.Zone,
			}, 0, 0)
			if err != nil {
				return err
			}
			t.tftp = tftpClient
		}
		// For boot tests, we need to add the appropriate boot args to the
		// specified custom images instead of the default images, so we'll pass
		// in only the custom images to bootserver.Boot() to exclude the default
		// images which already have boot args attached to them.
		var finalImgs []bootserver.Image
		var zbi, fvm *bootserver.Image
		if isBootTest {
			// Only get images from product bundle for boot tests when
			// loading through zedboot. Regular tests should just use
			// the images from images.json as is.
			zbi, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "zbi", "")
			if err != nil {
				return err
			}
			if zbi != nil {
				zbi.Args = []string{"--boot"}
				finalImgs = append(finalImgs, *zbi)
			}
			fvm, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fvm", "")
			if err != nil {
				return err
			}
			if fvm != nil {
				fvm.Args = []string{"--fvm"}
				finalImgs = append(finalImgs, *fvm)
			}
		}
		if len(finalImgs) == 0 {
			for _, img := range images {
				finalImgs = append(finalImgs, img)
			}
		}
		if err := bootserver.Boot(ctx, t.Tftp(), finalImgs, args, authorizedKeys); err != nil {
			return err
		}
	}

	if serialSocketPath != "" {
		if err := <-bootedLogChan; err != nil {
			return err
		}
		if t.opts.ExpectsSSH && useFastbootFlashing {
			if err := retry.Retry(ctx, retry.WithMaxAttempts(retry.NewConstantBackoff(5*time.Second), 5), func() error {
				return t.provisionSSHKey(ctx)
			}, nil); err != nil {
				return err
			}
		}
		return nil
	}

	return nil
}

func getImageByName(imgs []bootserver.Image, name string) *bootserver.Image {
	for _, img := range imgs {
		if img.Name == name {
			return &img
		}
	}
	return nil
}

// Images are not guaranteed to be uniquely identified by label.
func getImage(imgs []bootserver.Image, label, typ string) *bootserver.Image {
	for _, img := range imgs {
		if img.Label == label && typ == img.Type {
			return &img
		}
	}
	return nil
}

func (t *Device) runFastboot(ctx context.Context, fastbootPath string, cmds [][]string) error {
	stdout, stderr, flush := botanist.NewStdioWriters(ctx, "fastboot")
	defer flush()
	for _, cmdArgs := range cmds {
		cmdArgs = append([]string{"-s", t.config.FastbootSernum}, cmdArgs...)
		cmd := exec.CommandContext(ctx, fastbootPath, cmdArgs...)
		cmd.Stdout = stdout
		cmd.Stderr = stderr
		logger.Debugf(ctx, "starting: %v", cmd.Args)
		if err := cmd.Run(); err != nil {
			return err
		}
	}
	return nil
}

func (t *Device) bootZedboot(ctx context.Context, images []bootserver.Image) error {
	fastboot := getImageByName(images, "exe.linux-x64_fastboot")
	if fastboot == nil {
		return errors.New("fastboot not found")
	}
	zbi := getImageByName(images, "zbi_zircon-r")
	vbmeta := getImageByName(images, "vbmeta_zircon-r")
	logger.Debugf(ctx, "zbi: %s, vbmeta: %s", zbi.Path, vbmeta.Path)
	zbiContents, err := os.ReadFile(zbi.Path)
	if err != nil {
		return err
	}
	vbmetaContents, err := os.ReadFile(vbmeta.Path)
	if err != nil {
		return err
	}
	combinedZBIVBMeta := filepath.Join(filepath.Dir(zbi.Path), "zedboot.combined")
	err = os.WriteFile(combinedZBIVBMeta, append(zbiContents, vbmetaContents...), 0666)
	if err != nil {
		return err
	}
	err = t.runFastboot(ctx, fastboot.Path, [][]string{{"boot", combinedZBIVBMeta}})
	logger.Debugf(ctx, "done booting zedboot")
	return err
}

func (t *Device) provisionSSHKey(ctx context.Context) error {
	logger.Debugf(ctx, "provisioning SSH key")
	serialSocketPath := t.SerialSocketPath()
	socket, err := serial.NewSocket(ctx, serialSocketPath)
	if err != nil {
		return err
	}
	defer socket.Close()

	p, err := os.ReadFile(t.opts.AuthorizedKey)
	if err != nil {
		return err
	}
	pubkey := string(p)
	pubkey = strings.TrimSuffix(pubkey, "\n")
	cmds := []serial.Command{
		{Cmd: []string{"echo", fmt.Sprintf("\"%s\"", pubkey), ">", "/data/ssh/authorized_keys"}},
		{Cmd: []string{"cat", "/data/ssh/authorized_keys"}},
	}
	waitChan := make(chan error, 1)
	go func() {
		truncCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
		_, readErr := iomisc.ReadUntilMatchString(truncCtx, socket, "cat /data/ssh/authorized_keys")
		if readErr == nil {
			_, readErr = iomisc.ReadUntilMatchString(truncCtx, socket, pubkey)
			cancel()
			if readErr == nil {
				logger.Infof(ctx, "successfully provisioned SSH key")
			} else if errors.Is(readErr, truncCtx.Err()) {
				logger.Warningf(ctx, "ssh key is corrupted")
			} else {
				logger.Errorf(ctx, "unexpected error checking for ssh key: %s", readErr)
			}
		}
		waitChan <- readErr
	}()
	if err := serial.RunCommands(ctx, socket, cmds); err != nil {
		return err
	}
	return <-waitChan
}

// fastbootFlash runs fastboot commands directly to flash the device.
func (t *Device) fastbootFlash(ctx context.Context, pbPath string, images []bootserver.Image) error {
	fastboot := getImageByName(images, "exe.linux-x64_fastboot")
	if fastboot == nil {
		return errors.New("fastboot not found")
	}

	zbi, err := t.ffx.GetImageFromPB(ctx, pbPath, "a", "zbi", "")
	if err != nil {
		return err
	}
	if zbi == nil {
		return fmt.Errorf("failed to find zbi image from product bundle %s", pbPath)
	}

	fvmImage, err := t.ffx.GetImageFromPB(ctx, pbPath, "a", "fvm", "")
	if err != nil {
		return err
	}
	if fvmImage == nil {
		fvmImage, err = t.ffx.GetImageFromPB(ctx, pbPath, "a", "fxfs", "")
		if err != nil {
			return err
		}
	}
	cmds := [][]string{
		{"flash", "boot_a", zbi.Path},
		{"flash", "boot_b", zbi.Path},
	}
	if fvmImage != nil {
		cmds = append(cmds, []string{"flash", "super", fvmImage.Path})
	}
	cmds = append(cmds, []string{"reboot"})
	if err := t.runFastboot(ctx, fastboot.Path, cmds); err != nil {
		return err
	}
	logger.Debugf(ctx, "done flashing")
	return nil
}

func (t *Device) flash(ctx context.Context, productBundle, target string, tcpFlash bool) error {
	// Print logs to avoid hitting the I/O timeout.
	ticker := time.NewTicker(2 * time.Minute)
	defer ticker.Stop()
	go func() {
		for range ticker.C {
			logger.Debugf(ctx, "still flashing...")
		}
	}()

	// TODO(https://fxbug.dev/42168777): Need support for ffx target flash for cuckoo tests.
	return t.ffx.Flash(ctx, target, "", productBundle, tcpFlash)
}

// Nasty hack to try to deal with the fact that some devices have real bad USB
// signal quality which results in dropping packets in a way the target doesn't
// recognize.
func (t *Device) hardPowerCycleAndPlaceInFastboot(ctx context.Context) error {
	// All of this should easily complete within a minute.
	ctx, cancel := context.WithTimeout(ctx, hardRecoveryTimeoutSecs*time.Second)
	defer cancel()
	serialSocketPath := t.SerialSocketPath()
	if serialSocketPath == "" {
		return errors.New("No serial socket configured; cannot force device into fastboot")
	}

	// Start a goroutine which just periodically sends `f\r\n` on the serial
	// console, which is the magic invocation which tells firmware to drop
	// into the fastboot idle loop.
	serialSpamContext, serialSpamCancel := context.WithCancel(ctx)
	defer serialSpamCancel()
	go spamFToSerial(serialSpamContext, serialSocketPath)

	// Ask DMS to hard power cycle this device.  This usually takes ~20 seconds.
	err := t.hardPowerCycle(ctx)
	if err != nil {
		return fmt.Errorf("dmc invocation failed: %w", err)
	}
	logger.Debugf(ctx, "dmc invocation completed successfully")

	// Start a goroutine which watches the serial logs for this device for
	// fastbootIdleSignature.  This is intrinsically racy no matter when we
	// start it -- if we do it before the power-cycle, we might pick up the
	// line emission due to a USB link renegotiation from before we cut
	// power.  If we do it after the power-cycle, we might not open the
	// socket quickly enough to see the `USB RESET` line.
	// We opt for the latter, combined with a 10-second timeout after which
	// we assume success and carry on anyway.  This whole flow is only
	// intended for use in an attempted error recovery path anyway.
	fastbootedLogChan := make(chan error)
	go func() {
		defer close(fastbootedLogChan)
		logger.Debugf(ctx, "watching serial for string that indicates device is in fastboot: %q", fastbootIdleSignature)
		socket, err := net.Dial("unix", serialSocketPath)
		if err != nil {
			fastbootedLogChan <- fmt.Errorf("%s: %w", serialconstants.FailedToOpenSerialSocketMsg, err)
			return
		}
		defer socket.Close()
		_, err = iomisc.ReadUntilMatchString(ctx, socket, fastbootIdleSignature)
		fastbootedLogChan <- err
	}()
	fastbootWaitCtx, fastbootWaitCtxCancel := context.WithTimeout(ctx, fastbootIdleWaitTimeoutSecs*time.Second)
	defer fastbootWaitCtxCancel()

	// Wait until either we have seen the device enumerate on USB again (as
	// evidenced by the serial log signature) or until 10 seconds have
	// elapsed (an upper bound on the amount of time it should take a
	// device to reach fastboot).
	var retval error
	select {
	case res := <-fastbootedLogChan:
		logger.Debugf(fastbootWaitCtx, "Log watcher returned %s", err)
		retval = res
		// We wait an additional two seconds here because at the
		// instant we see this log line, the host is still enumerating
		// the device and has likely not finished all of its USB
		// requests yet, and `ffx flash` will error out immediately if
		// it can't find the device with the matching serial number,
		// rather than wait for such a device to appear (which is what
		// `fastboot` would do).
		// Feel free to remove this if `ffx flash` ever gets around to
		// doing the more-useful thing.
		time.Sleep(2 * time.Second)
	case <-fastbootWaitCtx.Done():
		logger.Debugf(fastbootWaitCtx, "Did not see '%s' in logs within %d seconds; carrying on anyway", fastbootIdleSignature, fastbootIdleWaitTimeoutSecs)
		retval = nil
	case <-ctx.Done():
		retval = fmt.Errorf("hard-reboot recovery did not complete within %d seconds", hardRecoveryTimeoutSecs)
	}

	return retval
}

func spamFToSerial(ctx context.Context, serialSocketPath string) {
	logger.Debugf(ctx, "starting serial spam to place device in fastboot")
	ticker := time.NewTicker(200 * time.Millisecond)
	defer ticker.Stop()
	socket, err := net.Dial("unix", serialSocketPath)
	if err != nil {
		logger.Errorf(ctx, "%s: %s", serialconstants.FailedToOpenSerialSocketMsg, err)
		return
	}
	defer socket.Close()

	// Start a routine to read from the socket we opened in the background,
	// lest the socket that never drains the kernel buffer eventually cause
	// the mux to block on writes, causing all other mux readers to stop
	// getting any new data as the channel buffers fill
	go func() {
		buf := make([]byte, 1024)
		for {
			if ctx.Err() != nil {
				return
			}
			_, err := socket.Read(buf)
			if err != nil {
				return
			}
		}
	}()

WriteLoop:
	for {
		select {
		case <-ctx.Done():
			break WriteLoop
		case <-ticker.C:
			msg := []byte{'\r', '\n', 'f', '\r', '\n'}
			socket.Write(msg)
		}
	}
	logger.Debugf(ctx, "stopping serial spam")
}

func (t *Device) hardPowerCycle(ctx context.Context) error {
	// there should be an env var DMC_PATH with the path to dmc
	cmdline := []string{
		os.Getenv("DMC_PATH"),
		"set-power-state",
		"-server-port",
		os.Getenv("DMS_PORT"),
		"-state",
		"cycle",
		"-nodename",
		t.Nodename(),
	}
	runner := subprocess.Runner{}
	// Run the dmc invocation and wait for the subprocess call to complete.
	// This usually takes ~20 seconds.
	return runner.Run(ctx, cmdline, subprocess.RunOptions{})
}

// Stop stops the device.
func (t *Device) Stop() error {
	t.genericFuchsiaTarget.Stop()
	atomic.StoreUint32(&t.stopping, 1)
	return nil
}

// Wait waits for the device target to stop.
func (t *Device) Wait(context.Context) error {
	return ErrUnimplemented
}

// Config returns fields describing the target.
func (t *Device) TestConfig(expectsSSH bool) (any, error) {
	return TargetInfo(t, expectsSSH, t.config.PDU)
}

func parseOutSigners(keyPaths []string) ([]ssh.Signer, error) {
	if len(keyPaths) == 0 {
		return nil, errors.New("must supply SSH keys in the config")
	}
	var keys [][]byte
	for _, keyPath := range keyPaths {
		p, err := os.ReadFile(keyPath)
		if err != nil {
			return nil, fmt.Errorf("could not read SSH key file %q: %w", keyPath, err)
		}
		keys = append(keys, p)
	}

	var signers []ssh.Signer
	for _, p := range keys {
		signer, err := ssh.ParsePrivateKey(p)
		if err != nil {
			return nil, err
		}
		signers = append(signers, signer)
	}
	return signers, nil
}

func (t *Device) neededForFlashing(img bootserver.Image) bool {
	if img.IsFlashable {
		return true
	}

	neededImages := []string{
		"exe.linux-x64_fastboot",
	}
	for _, imageName := range neededImages {
		if img.Name == imageName {
			return true
		}
	}
	return false
}
