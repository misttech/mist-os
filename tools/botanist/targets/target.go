// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/lib/repo"
	"go.fuchsia.dev/fuchsia/tools/bootserver"
	"go.fuchsia.dev/fuchsia/tools/botanist"
	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/build"
	"go.fuchsia.dev/fuchsia/tools/lib/ffxutil"
	"go.fuchsia.dev/fuchsia/tools/lib/iomisc"
	"go.fuchsia.dev/fuchsia/tools/lib/jsonutil"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/osmisc"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
	"go.fuchsia.dev/fuchsia/tools/lib/serial"
	"go.fuchsia.dev/fuchsia/tools/lib/syslog"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"golang.org/x/sync/errgroup"
)

const (
	localhostPlaceholder = "localhost"
	repoID               = "fuchsia-pkg://fuchsia.com"
)

// FFXInstance is a wrapper around ffxutil.FFXInstance with extra fields to
// determine how botanist uses ffx.
type FFXInstance struct {
	*ffxutil.FFXInstance
	// Experiments specify what experiments to enable.
	Experiments botanist.Experiments
}

// Base represents a device used during testing.
type Base interface {
	// TestConfig returns fields describing the target to be provided to tests
	// during runtime.
	//
	// This information will be provided as part of the testbed config. The path
	// to the testbed config will be available during test runtime via the
	// FUCHSIA_TESTBED_CONFIG environment variable.
	TestConfig(expectsSSH bool) (any, error)
}

// FuchsiaTarget is implemented for Fuchsia targets.
type FuchsiaTarget interface {
	Base

	// AddPackageRepository adds a given package repository to the target.
	AddPackageRepository(client *sshutil.Client, repoURL, blobURL string) error

	// CaptureSerialLog starts capturing serial logs to the given file.
	// This is only valid once the target has a serial multiplexer running.
	CaptureSerialLog(filename string) error

	// CaptureSyslog starts capturing the syslog to the given file.
	// This is only valid when the target has SSH running.
	CaptureSyslog(client *sshutil.Client, filename, repoURL, blobURL string) error

	// StopSyslog stops the syslog context that controls the process to capture the syslog
	// that was started by CaptureSyslog().
	StopSyslog()

	// IPv4 returns the IPv4 of the target; this is nil unless explicitly
	// configured.
	IPv4() (net.IP, error)

	// IPv6 returns the IPv6 of the target.
	IPv6() (*net.IPAddr, error)

	// Nodename returns the name of the target node.
	Nodename() string

	// Serial returns the serial device associated with the target for serial i/o.
	Serial() io.ReadWriteCloser

	// SerialSocketPath returns the path to the target's serial socket.
	SerialSocketPath() string

	// SetConnectionTimeout sets the timeout for making an SSH connection or resolving
	// the IP address.
	SetConnectionTimeout(time.Duration)

	// SSHClient returns an SSH client to the device (if the device has SSH running).
	SSHClient() (*sshutil.Client, error)

	// SSHKey returns the private key corresponding an authorized SSH key of the target.
	SSHKey() string

	// Start starts the target.
	Start(ctx context.Context, images []bootserver.Image, args []string, pbPath string, isBootTest bool) error

	// StartSerialServer starts the serial server for the target iff one
	// does not exist.
	StartSerialServer() error

	// Stop stops the target.
	Stop() error

	// Wait waits for the target to finish running.
	Wait(context.Context) error

	// SetFFX attaches an ffx instance and env to the target.
	SetFFX(*FFXInstance, []string)

	// GetFFX returns the ffx instance associated with the target.
	GetFFX() *FFXInstance

	// UseFFXExperiment returns whether to enable an experimental ffx feature.
	UseFFXExperiment(botanist.Experiment) bool

	// UseProductBundles returns whether this target can be provisioned using
	// product bundles.
	UseProductBundles() bool

	// FFXEnv returns the env vars that the ffx instance should run with
	FFXEnv() []string
}

// genericFuchsiaTarget is a generic Fuchsia instance.
// It is not intended to be instantiated directly, but rather embedded into a
// more concrete implementation.
type genericFuchsiaTarget struct {
	targetCtx       context.Context
	targetCtxCancel context.CancelFunc

	// stopSyslog is the function to cancel streaming the syslog.
	// This will be set by CaptureSyslog().
	stopSyslog func()

	nodename          string
	serial            io.ReadWriteCloser
	serialSocket      string
	sshKeys           []string
	connectionTimeout time.Duration

	ipv4         net.IP
	ipv6         *net.IPAddr
	serialServer *serial.Server

	ffx    *FFXInstance
	ffxEnv []string
}

// newGenericFuchsia creates a new generic Fuchsia target.
func newGenericFuchsia(ctx context.Context, nodename, serialSocket string, sshKeys []string, serial io.ReadWriteCloser) (*genericFuchsiaTarget, error) {
	targetCtx, cancel := context.WithCancel(ctx)
	t := &genericFuchsiaTarget{
		targetCtx:       targetCtx,
		targetCtxCancel: cancel,

		nodename:     nodename,
		serial:       serial,
		serialSocket: serialSocket,
		sshKeys:      sshKeys,
	}
	return t, nil
}

// SetFFX attaches an FFXInstance and environment to the target.
func (t *genericFuchsiaTarget) SetFFX(ffx *FFXInstance, env []string) {
	t.ffx = ffx
	t.ffxEnv = env
}

// GetFFX returns the FFXInstance associated with the target.
func (t *genericFuchsiaTarget) GetFFX() *FFXInstance {
	return t.ffx
}

// UseFFXExperiment returns whether the provided experiment is enabled.
// Use to enable experimental ffx features.
func (t *genericFuchsiaTarget) UseFFXExperiment(experiment botanist.Experiment) bool {
	return t.ffx.Experiments.Contains(experiment)
}

// UseProductBundles returns whether this target can be provisioned using
// product bundles. The default is true and should be overridden by each
// target type.
func (t *genericFuchsiaTarget) UseProductBundles() bool {
	return true
}

// FFXEnv returns the environment to run ffx with.
func (t *genericFuchsiaTarget) FFXEnv() []string {
	return t.ffxEnv
}

// StartSerialServer spawns a new serial server fo the given target.
// This is a no-op if a serial socket already exists, or if there is
// no attached serial device.
func (t *genericFuchsiaTarget) StartSerialServer() error {
	// We have to no-op instead of returning an error as there are code
	// paths that directly write to the serial log using QEMU's chardev
	// flag, and throwing an error here would break those paths.
	if t.serial == nil || t.serialSocket != "" {
		return nil
	}
	t.serialSocket = createSocketPath()
	t.serialServer = serial.NewServer(t.serial, serial.ServerOptions{})
	addr := &net.UnixAddr{
		Name: t.serialSocket,
		Net:  "unix",
	}
	l, err := net.ListenUnix("unix", addr)
	if err != nil {
		return err
	}
	go func() {
		t.serialServer.Run(t.targetCtx, l)
	}()
	return nil
}

// resolveIP uses mDNS to resolve the IPv6 and IPv4 addresses of the
// target. It then caches the results so future requests are fast.
func (t *genericFuchsiaTarget) resolveIP() error {
	timeout := 2 * time.Minute
	if t.connectionTimeout != 0 {
		timeout = t.connectionTimeout
	}
	ctx, cancel := context.WithTimeout(t.targetCtx, timeout)
	defer cancel()
	ipv4, ipv6, err := ResolveIP(ctx, t.nodename)
	if err != nil {
		return err
	}
	t.ipv4 = ipv4
	t.ipv6 = &ipv6
	return nil
}

// SerialSocketPath returns the path to the unix socket multiplexing serial
// logs.
func (t *genericFuchsiaTarget) SerialSocketPath() string {
	return t.serialSocket
}

// IPv4 returns the IPv4 address of the target.
func (t *genericFuchsiaTarget) IPv4() (net.IP, error) {
	if t.ipv4 == nil {
		if err := t.resolveIP(); err != nil {
			return nil, err
		}
	}
	return t.ipv4, nil
}

// IPv6 returns the IPv6 address of the target.
func (t *genericFuchsiaTarget) IPv6() (*net.IPAddr, error) {
	if t.ipv6 == nil {
		if err := t.resolveIP(); err != nil {
			return nil, err
		}
	}
	return t.ipv6, nil
}

// IPAddr returns the IP address of the target, favoring IPv4 over IPv6.
func IPAddr(t FuchsiaTarget) (net.IPAddr, error) {
	var addr net.IPAddr
	ipv6, err := t.IPv6()
	if err != nil {
		return addr, err
	}
	if ipv6 != nil {
		addr = *ipv6
	}
	ipv4, err := t.IPv4()
	if err != nil {
		return addr, err
	}
	if ipv4 != nil {
		addr.IP = ipv4
		addr.Zone = ""
	}
	return addr, nil
}

// CaptureSerialLog starts copying serial logs from the serial server
// to the given filename. This is a blocking function; it will not return
// until either the serial server disconnects or the target is stopped.
func (t *genericFuchsiaTarget) CaptureSerialLog(filename string) error {
	if t.serialSocket == "" {
		return errors.New("CaptureSerialLog() failed; serialSocket was empty")
	}
	serialLog, err := os.Create(filename)
	if err != nil {
		return err
	}
	serialLogWriter := botanist.NewTimestampWriter(serialLog)
	conn, err := net.Dial("unix", t.serialSocket)
	if err != nil {
		return err
	}
	// Set up a goroutine to terminate this capture on target context cancel.
	go func() {
		<-t.targetCtx.Done()
		conn.Close()
		serialLog.Close()
	}()

	// Start capturing serial logs.
	b := bufio.NewReader(conn)
	for {
		line, err := b.ReadString('\n')
		if err != nil {
			if !errors.Is(err, net.ErrClosed) {
				return fmt.Errorf("%s: %w", constants.SerialReadErrorMsg, err)
			}
			return nil
		}
		if _, err := io.WriteString(serialLogWriter, line); err != nil {
			return fmt.Errorf("failed to write line to serial log: %w", err)
		}
	}
}

// SetConnectionTimeout sets the timeout for making an SSH connection or
// resolving the IP address.
func (t *genericFuchsiaTarget) SetConnectionTimeout(timeout time.Duration) {
	t.connectionTimeout = timeout
}

// sshClient is a helper function that returns an SSH client connected to the
// target, which can be found at the given address.
func (t *genericFuchsiaTarget) sshClient(addr *net.IPAddr, connName string) (*sshutil.Client, error) {
	if len(t.sshKeys) == 0 {
		return nil, errors.New("SSHClient() failed; no ssh keys provided")
	}

	p, err := os.ReadFile(t.sshKeys[0])
	if err != nil {
		return nil, err
	}
	config, err := sshutil.DefaultSSHConfig(p)
	if err != nil {
		return nil, err
	}
	connectBackoff := sshutil.DefaultConnectBackoff()
	if t.connectionTimeout != 0 {
		connectBackoff = retry.WithMaxDuration(retry.NewConstantBackoff(time.Second), t.connectionTimeout)
	}
	return sshutil.NewNamedClient(
		t.targetCtx,
		sshutil.ConstantAddrResolver{
			Addr: &net.TCPAddr{
				IP:   addr.IP,
				Zone: addr.Zone,
				Port: sshutil.SSHPort,
			},
		},
		config,
		connectBackoff,
		connName,
	)
}

func (t *genericFuchsiaTarget) SSHClient() (*sshutil.Client, error) {
	// This should be implemented by the various FuchsiaTarget types.
	return nil, fmt.Errorf("SSHClient() not implemented")
}

// AddPackageRepository adds the given package repository to the target.
func (t *genericFuchsiaTarget) AddPackageRepository(client *sshutil.Client, repoURL, blobURL string) error {
	localhost := strings.Contains(repoURL, localhostPlaceholder) || strings.Contains(blobURL, localhostPlaceholder)
	lScopedRepoURL := repoURL
	if localhost {
		localAddr := client.LocalAddr()
		if localAddr == nil {
			return fmt.Errorf("failed to get local addr for ssh client")
		}
		host := localScopedLocalHost(localAddr.String())
		lScopedRepoURL = strings.Replace(repoURL, localhostPlaceholder, host, 1)
		logger.Infof(t.targetCtx, "local-scoped package repository address: %s\n", lScopedRepoURL)
	}

	rScopedRepoURL := repoURL
	rScopedBlobURL := blobURL
	if localhost {
		host, err := remoteScopedLocalHost(t.targetCtx, client)
		if err != nil {
			return err
		}
		rScopedRepoURL = strings.Replace(repoURL, localhostPlaceholder, host, 1)
		logger.Infof(t.targetCtx, "remote-scoped package repository address: %s\n", rScopedRepoURL)
		rScopedBlobURL = strings.Replace(blobURL, localhostPlaceholder, host, 1)
		logger.Infof(t.targetCtx, "remote-scoped package blob address: %s\n", rScopedBlobURL)
	}

	rootMeta, err := repo.GetRootMetadataInsecurely(t.targetCtx, lScopedRepoURL)
	if err != nil {
		return fmt.Errorf("failed to derive root metadata: %w", err)
	}

	cfg := &repo.Config{
		URL:           repoID,
		RootKeys:      rootMeta.RootKeys,
		RootVersion:   rootMeta.RootVersion,
		RootThreshold: rootMeta.RootThreshold,
		Mirrors: []repo.MirrorConfig{
			{
				URL:     rScopedRepoURL,
				BlobURL: rScopedBlobURL,
			},
		},
	}

	return repo.AddFromConfig(t.targetCtx, client, cfg)
}

// CaptureSyslog collects the target's syslog in the given file.
// This requires SSH to be running on the target. We pass the repoURL
// and blobURL of the package repo as a matter of convenience - it makes
// it easy to re-register the package repository on reboot. This function
// blocks until the target is stopped.
func (t *genericFuchsiaTarget) CaptureSyslog(client *sshutil.Client, filename, repoURL, blobURL string) error {
	var syslogger *syslog.Syslogger
	// The SSH client is no longer needed if using `ffx log`, so close it so
	// it doesn't keep sending keepalives.
	client.Close()
	syslogger = syslog.NewFFXSyslogger(t.ffx.FFXInstance)

	f, err := os.Create(filename)
	if err != nil {
		return err
	}
	defer f.Close()

	syslogWriter := botanist.NewLineWriter(botanist.NewTimestampWriter(f), "")
	syslogCtx, cancel := context.WithCancel(t.targetCtx)
	t.stopSyslog = func() {
		cancel()
		done := false
		timeout := time.After(5 * time.Second)
		for {
			select {
			case <-timeout:
				done = true
			default:
				if !syslogger.IsRunning() {
					done = true
				}
				time.Sleep(500 * time.Millisecond)
			}
			if done {
				break
			}
		}
	}
	defer t.stopSyslog()
	errs := syslogger.Stream(syslogCtx, t.targetCtx, syslogWriter)
	maxAttempts := 5
	startTime := time.Now()
	attempt := 0
	for range errs {
		attempt += 1
		if attempt == maxAttempts && time.Since(startTime) < time.Minute {
			// If we failed maxAttempts times within a minute of starting to stream,
			// then there's likely an issue with the syslogger so return an err.
			// LINT.IfChange(syslog_failed)
			return fmt.Errorf("failed to stream syslog multiple times within 1 minute: %d attempts", maxAttempts)
			// LINT.ThenChange(/tools/testing/tefmocheck/string_in_log_check.go:syslog_failed)
		}
		if !syslogger.IsRunning() {
			return nil
		}
		// TODO(rudymathu): This is a bit of a hack that results from the fact that
		// we don't know when test binaries restart the device. Eventually, we should
		// build out a more resilient framework in which we register "restart handlers"
		// that are triggered on reboot.
		if repoURL != "" && blobURL != "" {
			select {
			case <-client.DisconnectionListener():
				if err := client.Reconnect(syslogCtx); err != nil {
					return fmt.Errorf("failed to reconnect SSH client: %w", err)
				}
			default:
				// The client is still connected, so continue.
			}
			t.AddPackageRepository(client, repoURL, blobURL)
			client.Close()
		}
	}
	return nil
}

func (t *genericFuchsiaTarget) StopSyslog() {
	if t.stopSyslog != nil {
		t.stopSyslog()
	}
}

// Stop cleans up all of the resources used by the target.
func (t *genericFuchsiaTarget) Stop() {
	// Cancelling the target context will stop any background goroutines.
	// This includes serial/syslog capture and any serial servers that may
	// be running.
	t.targetCtxCancel()
}

func copyImagesToDir(ctx context.Context, dir string, preservePath bool, imgs ...*bootserver.Image) error {
	// Copy each in a goroutine for efficiency's sake.
	eg, ctx := errgroup.WithContext(ctx)
	for _, img := range imgs {
		if img != nil {
			img := img
			eg.Go(func() error {
				base := img.Name
				if preservePath {
					base = img.Path
				}
				dest := filepath.Join(dir, base)
				return bootserver.DownloadWithRetries(ctx, dest, func() error {
					return copyImageToDir(ctx, dest, img)
				})
			})
		}
	}
	return eg.Wait()
}

func copyImageToDir(ctx context.Context, dest string, img *bootserver.Image) error {
	f, ok := img.Reader.(*os.File)
	if ok {
		if err := osmisc.CopyFile(f.Name(), dest); err != nil {
			return err
		}
		img.Path = dest
		return nil
	}

	f, err := osmisc.CreateFile(dest)
	if err != nil {
		return err
	}
	defer f.Close()

	// Log progress to avoid hitting I/O timeout in case of slow transfers.
	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()
	go func() {
		for range ticker.C {
			logger.Debugf(ctx, "transferring %s...\n", img.Name)
		}
	}()

	if _, err := io.Copy(f, iomisc.ReaderAtToReader(img.Reader)); err != nil {
		return fmt.Errorf("%s (%q): %w", constants.FailedToCopyImageMsg, img.Name, err)
	}
	img.Path = dest

	if img.IsExecutable {
		if err := os.Chmod(img.Path, os.ModePerm); err != nil {
			return fmt.Errorf("failed to make %s executable: %w", img.Path, err)
		}
	}

	// We no longer need the reader at this point.
	if c, ok := img.Reader.(io.Closer); ok {
		c.Close()
	}
	img.Reader = nil
	return nil
}

func localScopedLocalHost(laddr string) string {
	tokens := strings.Split(laddr, ":")
	host := strings.Join(tokens[:len(tokens)-1], ":") // Strips the port.
	return escapePercentSign(host)
}

func remoteScopedLocalHost(ctx context.Context, client *sshutil.Client) (string, error) {
	// From the ssh man page:
	// "SSH_CONNECTION identifies the client and server ends of the connection.
	// The variable contains four space-separated values: client IP address,
	// client port number, server IP address, and server port number."
	// We wish to obtain the client IP address, as will be scoped from the
	// remote address.
	var stdout bytes.Buffer
	if err := client.Run(ctx, []string{"echo", "${SSH_CONNECTION}"}, &stdout, nil); err != nil {
		return "", fmt.Errorf("failed to derive $SSH_CONNECTION: %w", err)
	}
	val := stdout.String()
	tokens := strings.Split(val, " ")
	if len(tokens) != 4 {
		return "", fmt.Errorf("$SSH_CONNECTION should be four space-separated values and not %q", val)
	}
	host := tokens[0]
	// If the host is an IPv6 address, surround it with brackets.
	if strings.Contains(host, ":") {
		host = "[" + escapePercentSign(host) + "]"
	}
	return host, nil
}

// From the spec https://tools.ietf.org/html/rfc6874#section-2:
// "%" is always treated as an escape character in a URI, so, according to
// the established URI syntax any occurrences of literal "%" symbols in a
// URI MUST be percent-encoded and represented in the form "%25".
func escapePercentSign(addr string) string {
	if strings.Contains(addr, "%25") {
		return addr
	}
	return strings.Replace(addr, "%", "%25", 1)
}

func createSocketPath() string {
	// We randomly construct a socket path that is highly improbable to collide with anything.
	randBytes := make([]byte, 16)
	rand.Read(randBytes)
	return filepath.Join(os.TempDir(), "serial"+hex.EncodeToString(randBytes)+".sock")
}

// Options represents lifecycle options for a target. The options will not necessarily make
// sense for all target types.
type Options struct {
	// Netboot gives whether to netboot or pave. Netboot here is being used in the
	// colloquial sense of only sending netsvc a kernel to mexec. If false, the target
	// will be paved. Ignored for QEMU.
	Netboot bool

	// ExpectsSSH specifies whether we expect to be able to SSH to the target.
	ExpectsSSH bool

	// SSHKey is a private SSH key file, corresponding to an authorized key to be paved or
	// to one baked into a boot image.
	SSHKey string

	// AuthorizedKey is the authorized key file corresponding to the private SSH key.
	AuthorizedKey string
}

// FromJSON parses a Base target from JSON config.
func FromJSON(ctx context.Context, config json.RawMessage, opts Options) (Base, error) {
	type typed struct {
		Type string `json:"type"`
	}
	var x typed

	if err := json.Unmarshal(config, &x); err != nil {
		return nil, fmt.Errorf("object in list has no \"type\" field: %w", err)
	}
	switch x.Type {
	case "aemu":
		var cfg EmulatorConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid QEMU config found: %w", err)
		}
		return NewAEMU(ctx, cfg, opts)
	case "qemu":
		var cfg EmulatorConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid QEMU config found: %w", err)
		}
		return NewQEMU(ctx, cfg, opts)
	case "crosvm":
		var cfg EmulatorConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid crosvm config found: %w", err)
		}
		return NewCrosvm(ctx, cfg, opts)
	case "device":
		var cfg DeviceConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid device config found: %w", err)
		}
		return NewDevice(ctx, cfg, opts)
	case "gce":
		var cfg GCEConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid GCE config found: %w", err)
		}
		return NewGCE(ctx, cfg, opts)
	case "auxiliary":
		var cfg AuxiliaryConfig
		if err := json.Unmarshal(config, &cfg); err != nil {
			return nil, fmt.Errorf("invalid auxiliary device config found: %w", err)
		}
		return NewAuxiliary(cfg)
	default:
		return nil, fmt.Errorf("unknown type found: %q", x.Type)
	}
}

// StartOptions bundles options for starting a target.
type StartOptions struct {
	// Netboot tells the image to use netboot if true; otherwise, pave.
	Netboot bool

	// ImageManifest is the path to an image manifest.
	ImageManifest string

	// ZirconArgs are kernel command-line arguments to pass to zircon on boot.
	ZirconArgs []string

	// ProductBundles is a path to product_bundles.json file.
	ProductBundles string

	// ProductBundleName is a name of product bundle getting used.
	ProductBundleName string

	// IsBootTest tells whether the provided product bundle is for a boot test.
	IsBootTest bool

	// BootupTimeout is the timeout to wait for an SSH connection after booting the target.
	BootupTimeout time.Duration
}

// StartTargets starts all the targets given the opts.
func StartTargets(ctx context.Context, opts StartOptions, targets []FuchsiaTarget) error {
	bootMode := bootserver.ModePave
	if opts.Netboot {
		bootMode = bootserver.ModeNetboot
	}

	// We wait until targets have started before running testrunner against the zeroth one.
	eg, startCtx := errgroup.WithContext(ctx)
	for _, t := range targets {
		t := t
		if opts.BootupTimeout > 0 {
			t.SetConnectionTimeout(opts.BootupTimeout)
		}
		eg.Go(func() error {
			imgs, closeFunc, err := bootserver.GetImages(startCtx, opts.ImageManifest, bootMode)
			if err != nil {
				return err
			}
			defer closeFunc()

			// Parse the product bundles
			var pbPath string
			if opts.ProductBundles != "" && t.UseProductBundles() {
				var productBundles []build.ProductBundle
				if err := jsonutil.ReadFromFile(opts.ProductBundles, &productBundles); err != nil {
					return err
				}

				pbPath = build.GetPbPathByName(productBundles, opts.ProductBundleName)
			}

			return t.Start(startCtx, imgs, opts.ZirconArgs, pbPath, opts.IsBootTest)
		})
	}
	return eg.Wait()
}

// StopTargets stop all the targets.
func StopTargets(ctx context.Context, targets []FuchsiaTarget) {
	// Stop the targets in parallel.
	var eg errgroup.Group
	for _, t := range targets {
		t := t
		eg.Go(func() error {
			return t.Stop()
		})
	}
	_ = eg.Wait()
}

// LINT.IfChange
type targetInfo struct {
	// Type identifies the type of device.
	Type string `json:"type"`

	// Nodename is the Fuchsia nodename of the target.
	Nodename string `json:"nodename"`

	// IPv4 is the IPv4 address of the target.
	IPv4 string `json:"ipv4"`

	// IPv6 is the IPv6 address of the target.
	IPv6 string `json:"ipv6"`

	// SerialSocket is the path to the serial socket, if one exists.
	SerialSocket string `json:"serial_socket"`

	// SSHKey is a path to a private key that can be used to access the target.
	SSHKey string `json:"ssh_key"`

	// PDU is an optional reference to the power distribution unit controlling
	// power delivery to the target. This will not always be present.
	PDU *targetPDU `json:"pdu,omitempty"`
}

type targetPDU struct {
	// IP is the IPv4 or IPv6 address of the PDU.
	IP string `json:"ip"`

	// MAC is the MAC address of the PDU.
	MAC string `json:"mac"`

	// Port is the PDU port index the target is connected to.
	Port uint8 `json:"port"`
}

// LINT.ThenChange(//src/testing/end_to_end/mobly_driver/api_mobly.py)

// TargetInfo returns config used to communicate with the target (device
// properties, serial paths, SSH properties, etc.) for use by subprocesses.
func TargetInfo(t FuchsiaTarget, expectsSSH bool, pdu *targetPDU) (targetInfo, error) {
	cfg := targetInfo{
		Type:         "FuchsiaDevice",
		Nodename:     t.Nodename(),
		SerialSocket: t.SerialSocketPath(),
		PDU:          pdu,
	}
	if !expectsSSH {
		return cfg, nil
	}

	cfg.SSHKey = t.SSHKey()
	if ipv4, err := t.IPv4(); err != nil {
		return cfg, err
	} else if ipv4 != nil {
		cfg.IPv4 = ipv4.String()
	}

	if ipv6, err := t.IPv6(); err != nil {
		return cfg, err
	} else if ipv6 != nil {
		cfg.IPv6 = ipv6.String()
	}

	return cfg, nil
}
