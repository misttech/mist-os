// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package device

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"sync/atomic"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/artifacts"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/build"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/packages"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/paver"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/sl4f"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
	"go.fuchsia.dev/fuchsia/tools/net/sshutil"
	"golang.org/x/crypto/ssh"
)

const rebootCheckPath = "/tmp/ota_test_should_reboot"

// Client manages the connection to the device.
type Client struct {
	deviceResolver           DeviceResolver
	sshClient                *sshutil.Client
	initialMonotonicTime     time.Time
	workaroundBrokenTimeSkip bool
	bootCounter              *uint32
	repoPort                 int
}

// NewClient creates a new Client.
func NewClient(
	ctx context.Context,
	repoPort int,
	deviceResolver DeviceResolver,
	privateKey ssh.Signer,
	sshConnectBackoff retry.Backoff,
	workaroundBrokenTimeSkip bool,
	serialConn *SerialConn,
	ffxTool *ffx.FFXTool,
) (*Client, error) {
	sshConfig, err := newSSHConfig(privateKey)
	if err != nil {
		return nil, err
	}

	sshClient, err := sshutil.NewClient(
		ctx,
		&addrResolver{
			deviceResolver: deviceResolver,
		},
		sshConfig,
		sshConnectBackoff,
	)
	if err != nil {
		return nil, err
	}

	bootCounter := new(uint32)
	if serialConn != nil {
		go func() {
			for {
				line, err := serialConn.ReadLine()
				if err != nil {
					logger.Errorf(ctx, "failed to read from serial: %v", err)
					break
				}
				if strings.HasSuffix(line, "Welcome to Zircon\n") {
					atomic.AddUint32(bootCounter, 1)
				}
			}
		}()
	}

	c := &Client{
		deviceResolver:           deviceResolver,
		sshClient:                sshClient,
		workaroundBrokenTimeSkip: workaroundBrokenTimeSkip,
		bootCounter:              bootCounter,
		repoPort:                 repoPort,
	}

	if err := c.postConnectSetup(ctx, ffxTool); err != nil {
		c.Close()
		return nil, err

	}

	return c, nil
}

// Construct a new `ssh.ClientConfig` for a given key file, or return an error if
// the key is invalid.
func newSSHConfig(privateKey ssh.Signer) (*ssh.ClientConfig, error) {
	config := &ssh.ClientConfig{
		User: "fuchsia",
		Auth: []ssh.AuthMethod{
			ssh.PublicKeys(privateKey),
		},
		HostKeyCallback: ssh.InsecureIgnoreHostKey(),
		Timeout:         30 * time.Second,
	}

	return config, nil
}

// Close the Client connection
func (c *Client) Close() {
	c.sshClient.Close()
}

// Run all setup steps after we've connected to a device.
func (c *Client) postConnectSetup(
	ctx context.Context,
	ffxTool *ffx.FFXTool,
) error {
	// TODO(https://fxbug.dev/42154680): The device might drop connections
	// early after boot when the RTC is updated, which typically happens
	// about 10 seconds after boot. To avoid this, if we find that we
	// connected before 15s, we'll disconnect, sleep, then connect again.
	if c.workaroundBrokenTimeSkip {
		logger.Infof(ctx, "Sleeping 15s in case https://fxbug.dev/42154685 causes a spurious disconnection")
		time.Sleep(15 * time.Second)

		if err := c.sshClient.Reconnect(ctx); err != nil {
			return err
		}
	}

	c.setInitialMonotonicTime(ctx, ffxTool)

	return nil
}

func (c *Client) Reconnect(ctx context.Context, ffxTool *ffx.FFXTool) error {
	if err := c.sshClient.Reconnect(ctx); err != nil {
		return err
	}

	return c.postConnectSetup(ctx, ffxTool)
}

func (c *Client) setInitialMonotonicTime(
	ctx context.Context,
	ffxTool *ffx.FFXTool,
) {
	nodeName := c.deviceResolver.NodeName()
	monotonicTime, err := ffxTool.TargetGetSshTime(ctx, nodeName)

	if err == nil {
		c.initialMonotonicTime = time.Now().Add(-monotonicTime)
	} else {
		logger.Warningf(ctx, "failed to get time with ffx: %v", err)
		logger.Warningf(ctx, "resetting time to zero")

		c.initialMonotonicTime = time.Time{}
	}
}

func (c *Client) getEstimatedMonotonicTime() time.Duration {
	if c.initialMonotonicTime.IsZero() {
		return 0
	}
	return time.Since(c.initialMonotonicTime)
}

// Run a command to completion on the remote device and write STDOUT and STDERR
// to the passed in io.Writers.
func (c *Client) Run(ctx context.Context, command []string, stdout io.Writer, stderr io.Writer) error {
	return c.sshClient.Run(ctx, command, stdout, stderr)
}

// DisconnectionListener returns a channel that is closed when the client is
// disconnected.
func (c *Client) DisconnectionListener() <-chan struct{} {
	return c.sshClient.DisconnectionListener()
}

func (c *Client) GetSSHConnection(ctx context.Context) (string, error) {
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd := []string{"PATH=''", "echo", "$SSH_CONNECTION"}
	if err := c.Run(ctx, cmd, &stdout, &stderr); err != nil {
		return "", fmt.Errorf("failed to read SSH_CONNECTION: %w: %s", err, string(stderr.Bytes()))
	}
	return strings.Split(string(stdout.Bytes()), " ")[0], nil
}

func (c *Client) GetSystemImageMerkle(ctx context.Context) (build.MerkleRoot, error) {
	const systemImageMeta = "/system/meta"
	merkleBytes, err := c.ReadRemotePath(ctx, systemImageMeta)
	if err != nil {
		return build.MerkleRoot{}, err
	}

	return build.DecodeMerkleRoot([]byte(strings.TrimSpace(string(merkleBytes))))
}

// Reboot asks the device to reboot. It waits until the device reconnects
// before returning.
func (c *Client) Reboot(ctx context.Context, ffxTool *ffx.FFXTool) error {
	logger.Infof(ctx, "rebooting")

	return c.ExpectReboot(ctx, ffxTool, func() error {
		// Run the reboot in the background, which gives us a chance to
		// observe us successfully executing the reboot command.
		return c.RunReboot(ctx)
	})
}

// RunReboot runs the reboot command
func (c *Client) RunReboot(ctx context.Context) error {
	cmd := []string{"dm", "reboot", "&", "exit", "0"}
	if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
		// If the device rebooted before ssh was able to tell
		// us the command ran, it will tell us the session
		// exited without passing along an exit code. So,
		// ignore that specific error.
		var exitErr *ssh.ExitMissingError
		if errors.As(err, &exitErr) {
			logger.Infof(ctx, "ssh disconnected before returning a status")
		} else {
			return fmt.Errorf("failed to reboot: %w", err)
		}
	}
	return nil
}

// RebootToBootloader asks the device to reboot into the bootloader. It
// waits until the device disconnects before returning.
func (c *Client) RebootToBootloader(ctx context.Context) error {
	logger.Infof(ctx, "Rebooting to bootloader")

	return c.ExpectDisconnect(ctx, func() error {
		// Run the reboot in the background, which gives us a chance to
		// observe us successfully executing the reboot command.
		cmd := []string{"dm", "reboot-bootloader", "&", "exit", "0"}
		if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
			// If the device rebooted before ssh was able to tell
			// us the command ran, it will tell us the session
			// exited without passing along an exit code. So,
			// ignore that specific error.
			var exitErr *ssh.ExitMissingError
			if errors.As(err, &exitErr) {
				logger.Infof(ctx, "ssh disconnected before returning a status")
			} else {
				return fmt.Errorf("failed to reboot into bootloader: %w", err)
			}
		}

		return nil
	})
}

// RebootToRecovery asks the device to reboot into the recovery partition. It
// waits until the device disconnects before returning.
func (c *Client) RebootToRecovery(ctx context.Context) error {
	logger.Infof(ctx, "Rebooting to recovery")

	return c.ExpectDisconnect(ctx, func() error {
		// Run the reboot in the background, which gives us a chance to
		// observe us successfully executing the reboot command.
		cmd := []string{"dm", "reboot-recovery", "&", "exit", "0"}
		if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
			// If the device rebooted before ssh was able to tell
			// us the command ran, it will tell us the session
			// exited without passing along an exit code. So,
			// ignore that specific error.
			var exitErr *ssh.ExitMissingError
			if errors.As(err, &exitErr) {
				logger.Infof(ctx, "ssh disconnected before returning a status")
			} else {
				return fmt.Errorf("failed to reboot into recovery: %w", err)
			}
		}

		return nil
	})
}

// Suspend asks the device to suspend. It waits until the device disconnects
// before returning.
func (c *Client) Suspend(ctx context.Context) error {
	logger.Infof(ctx, "Suspending")

	return c.ExpectDisconnect(ctx, func() error {
		// Run the suspend in the background, which gives us a chance to
		// observe us successfully executing the suspend command.
		cmd := []string{"dm", "suspend", "&", "exit", "0"}
		if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
			// If the device suspends before ssh was able to tell
			// us the command ran, it will tell us the session
			// exited without passing along an exit code. So,
			// ignore that specific error.
			var exitErr *ssh.ExitMissingError
			if errors.As(err, &exitErr) {
				logger.Infof(ctx, "ssh disconnected before returning a status")
			} else {
				return fmt.Errorf("failed to suspend: %w", err)
			}
		}

		return nil
	})
}

func (c *Client) ExpectDisconnect(ctx context.Context, f func() error) error {
	ch := c.DisconnectionListener()

	if err := f(); err != nil {
		return err
	}

	// Wait until we get a signal that we have disconnected
	select {
	case <-ch:
	case <-ctx.Done():
		return fmt.Errorf("device did not disconnect: %w", ctx.Err())
	}

	logger.Infof(ctx, "device disconnected")

	return nil
}

// ExpectReboot prepares a device for a reboot, runs a closure `f` that should
// reboot the device, then finally verifies whether a reboot actually took
// place. It does this by writing a unique value to
// `/tmp/ota_test_should_reboot`, then executing the closure. After we
// reconnect, we check if `/tmp/ota_test_should_reboot` exists. If not, exit
// with `nil`. Otherwise, we failed to reboot, or some competing test is also
// trying to reboot the device. Either way, err out.
func (c *Client) ExpectReboot(
	ctx context.Context,
	ffxTool *ffx.FFXTool,
	f func() error,
) error {
	// Generate a unique value.
	b := make([]byte, 16)
	_, err := rand.Read(b)
	if err != nil {
		return fmt.Errorf("failed to generate a unique boot number: %w", err)
	}

	// Encode the id into hex so we can write it through the shell.
	bootID := hex.EncodeToString(b)

	// Write the value to the file. Err if the file already exists by setting the
	// noclobber setting.
	cmd := fmt.Sprintf(
		`(
			set -C &&
			PATH= echo "%s" > "%s"
        )`, bootID, rebootCheckPath)

	if err := c.Run(ctx, strings.Fields(cmd), os.Stdout, os.Stderr); err != nil {
		return fmt.Errorf("failed to write reboot check file: %w", err)
	}

	// As a sanity check, make sure the file actually exists and has the correct
	// value.
	b, err = c.ReadRemotePath(ctx, rebootCheckPath)
	if err != nil {
		return fmt.Errorf("failed to read reboot check file: %w", err)
	}
	actual := strings.TrimSpace(string(b))

	if actual != bootID {
		return fmt.Errorf("reboot check file has wrong value: expected %q, got %q", bootID, actual)
	}

	// Look up the boot count before we reboot the device.
	initialBootCount := *c.bootCounter

	ch := c.DisconnectionListener()

	if err := f(); err != nil {
		return err
	}

	// Wait until we get a signal that we have disconnected
	select {
	case <-ch:
	case <-ctx.Done():
		return fmt.Errorf("device did not disconnect: %w", ctx.Err())
	}

	logger.Infof(ctx, "device disconnected, waiting for device to boot")

	if err := c.Reconnect(ctx, ffxTool); err != nil {
		return fmt.Errorf("failed to reconnect: %w", err)
	}

	// We've reconnected to the device, so count how many times we've rebooted.
	afterBootCount := *c.bootCounter

	// If we have boot counting enabled (signified by the initial boot
	// count not being zero), then check how many times we rebooted. It
	// should be 1 more than our initial count. If not, error out.
	logger.Infof(ctx, "device appears to have rebooted %d times", afterBootCount-initialBootCount)
	if initialBootCount != 0 && initialBootCount+1 != afterBootCount {
		return fmt.Errorf("device appears to have rebooted more than once! %d != %d", initialBootCount, afterBootCount)
	}

	// We reconnected to the device. Check that the reboot check file doesn't exist.
	exists, err := c.RemoteFileExists(ctx, rebootCheckPath)
	if err != nil {
		return fmt.Errorf(`failed to check if %q exists: %w`, rebootCheckPath, err)
	}
	if exists {
		// The reboot file exists. This could have happened because either we
		// didn't reboot, or some other test is also trying to reboot the
		// device. We can distinguish the two by comparing the file contents
		// with the bootID we wrote earlier.
		b, err := c.ReadRemotePath(ctx, rebootCheckPath)
		if err != nil {
			return fmt.Errorf("failed to read reboot check file: %w", err)
		}
		actual := strings.TrimSpace(string(b))

		// If the contents match, then we failed to reboot.
		if actual == bootID {
			return fmt.Errorf("reboot check file exists after reboot, device did not reboot")
		}

		return fmt.Errorf(
			"reboot check file exists after reboot, and has unexpected value: expected %q, got %q",
			bootID,
			actual,
		)
	}

	return nil
}

// ValidateStaticPackages checks that all static packages have no missing blobs.
func (c *Client) ValidateStaticPackages(ctx context.Context) error {
	logger.Infof(ctx, "validating static packages")

	path := "/pkgfs/ctl/validation/missing"
	f, err := c.ReadRemotePath(ctx, path)
	if err != nil {
		return fmt.Errorf("error reading %q: %w", path, err)
	}

	merkles := strings.TrimSpace(string(f))
	if merkles != "" {
		return fmt.Errorf("static packages are missing the following blobs:\n%s", merkles)
	}

	logger.Infof(ctx, "all static package blobs are accounted for")
	return nil
}

// ReadRemotePath read a file off the remote device.
func (c *Client) ReadRemotePath(ctx context.Context, path string) ([]byte, error) {
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd := fmt.Sprintf(
		`(
		test -e "%s" &&
		while IFS='' read f; do
			echo "$f";
		done < "%s" &&
		if [ ! -z "$f" ];
			then echo "$f";
		fi
		)`, path, path)
	if err := c.Run(ctx, strings.Fields(cmd), &stdout, &stderr); err != nil {
		return nil, fmt.Errorf("failed to read %q: %w: %s", path, err, string(stderr.Bytes()))
	}

	return stdout.Bytes(), nil
}

// DeleteRemotePath deletes a file off the remote device.
func (c *Client) DeleteRemotePath(ctx context.Context, path string) error {
	var stderr bytes.Buffer
	cmd := []string{"PATH=''", "rm", path}
	if err := c.Run(ctx, cmd, os.Stdout, &stderr); err != nil {
		return fmt.Errorf("failed to delete %q: %w: %s", path, err, string(stderr.Bytes()))
	}

	return nil
}

// RemoteFileExists checks if a file exists on the remote device.
func (c *Client) RemoteFileExists(ctx context.Context, path string) (bool, error) {
	var stderr bytes.Buffer
	cmd := []string{"PATH=''", "test", "-e", path}

	if err := c.Run(ctx, cmd, io.Discard, &stderr); err != nil {
		if e, ok := err.(*ssh.ExitError); ok {
			if e.ExitStatus() == 1 {
				return false, nil
			}
		}
		return false, fmt.Errorf("error reading %q: %w: %s", path, err, string(stderr.Bytes()))
	}

	return true, nil
}

// RegisterPackageRepository adds the repository as a repository inside the device.
// If rewritePackages is not nil, the rewrite rule will only affect the passed packages.
func (c *Client) RegisterPackageRepository(
	ctx context.Context,
	repo *packages.Server,
	repoName string,
	createRewriteRule bool,
	rewritePackages []string,
) error {
	logger.Infof(ctx, "registering package repository: %s", repo.Dir)

	if createRewriteRule {
		cmd := []string{"pkgctl", "repo", "add", "url", "-n", repoName, repo.URL}
		if err := c.Run(ctx, cmd, os.Stdout, os.Stderr); err != nil {
			return err
		}
		logger.Infof(ctx, "establishing rewriting rule for: %s", repo.URL)
		ruleTemplate := `'{"version":"1","content":[
			{"host_match":"fuchsia.com","host_replacement":"%[1]v","path_prefix_match":"/","path_prefix_replacement":"/"},
			{"host_match":"chromium.org","host_replacement":"%[1]v","path_prefix_match":"/","path_prefix_replacement":"/"}
		]}'`
		if rewritePackages != nil {
			ruleTemplate = `'{"version":"1","content":[`
			for i, p := range rewritePackages {
				if i > 0 {
					ruleTemplate += ","
				}
				ruleTemplate += `{
					"host_match":"fuchsia.com",
					"host_replacement":"%[1]v",
					"path_prefix_match":"/` + p + `",
					"path_prefix_replacement":"/` + p + `"
				}, {
					"host_match":"fuchsia.com",
					"host_replacement":"%[1]v",
					"path_prefix_match":"/` + p + `/0",
					"path_prefix_replacement":"/` + p + `/0"
				}`
			}
			ruleTemplate += `]}'`
		}
		cmd = []string{"pkgctl", "rule", "replace", "json", fmt.Sprintf(ruleTemplate, repoName)}
		return c.Run(ctx, cmd, os.Stdout, os.Stderr)
	} else {
		cmd := []string{"pkgctl", "repo", "add", "url", repo.URL}
		return c.Run(ctx, cmd, os.Stdout, os.Stderr)
	}
}

func (c *Client) ServePackageRepository(
	ctx context.Context,
	repo *packages.Repository,
	repoName string,
	createRewriteRule bool,
	rewritePackages []string,
) (*packages.Server, error) {
	// Make sure the device doesn't have any broken static packages.
	if err := c.ValidateStaticPackages(ctx); err != nil {
		return nil, err
	}

	// Tell the device to connect to our repository.
	localHostname, err := c.GetSSHConnection(ctx)
	if err != nil {
		return nil, err
	}

	// Serve the repository before the test begins.
	server, err := repo.Serve(ctx, localHostname, repoName, c.repoPort)
	if err != nil {
		return nil, err
	}

	if err := c.RegisterPackageRepository(ctx, server, repoName, createRewriteRule, rewritePackages); err != nil {
		server.Shutdown(ctx)
		return nil, err
	}

	return server, nil
}

func (c *Client) StartRpcSession(ctx context.Context, repo *packages.Repository) (*sl4f.Client, error) {
	logger.Infof(ctx, "connecting to sl4f")
	startTime := time.Now()

	// Configure the target to use this repository as "fuchsia-pkg://host_target_testing_sl4f".
	repoName := "host-target-testing-sl4f"
	repoServer, err := c.ServePackageRepository(ctx, repo, repoName, true, []string{"sl4f", "start_sl4f"})
	if err != nil {
		return nil, fmt.Errorf("error serving repo to device: %w", err)
	}
	defer repoServer.Shutdown(ctx)

	sshAddr, err := c.deviceResolver.ResolveSshAddress(ctx)
	if err != nil {
		return nil, fmt.Errorf("error resolving device host: %w", err)
	}

	deviceHostname, _, err := net.SplitHostPort(sshAddr)
	if err != nil {
		return nil, fmt.Errorf("error parsing ssh address %v: %w", sshAddr, err)
	}

	rpcClient, err := sl4f.NewClient(ctx, c.sshClient, net.JoinHostPort(deviceHostname, "80"), "fuchsia.com")
	if err != nil {
		return nil, fmt.Errorf("error creating sl4f client: %w", err)
	}

	logger.Infof(ctx, "connected to sl4f in %s", time.Now().Sub(startTime))

	return rpcClient, nil
}

// Pave paves the device to the specified build. It assumes the device is
// already in recovery, since there are multiple ways to get a device into
// recovery. Does not reconnect to the device.
func (c *Client) Pave(
	ctx context.Context,
	build artifacts.Build,
	sshPublicKey ssh.PublicKey,
) error {
	p, err := build.GetPaver(ctx, sshPublicKey)
	if err != nil {
		return fmt.Errorf("failed to get paver to pave device: %w", err)
	}

	if err := c.RebootToRecovery(ctx); err != nil {
		return fmt.Errorf("failed to reboot to recovery during paving: %w", err)
	}

	// First, pave the build's zedboot onto the device.
	logger.Infof(ctx, "waiting for device to enter zedboot")
	listeningName, err := c.deviceResolver.WaitToFindDeviceInNetboot(ctx)
	if err != nil {
		return fmt.Errorf("failed to wait for device to reboot into zedboot: %w", err)
	}

	if err = p.PaveWithOptions(ctx, listeningName, paver.Options{Mode: paver.ZedbootOnly}); err != nil {
		return fmt.Errorf("device failed to pave: %w", err)
	}

	// Next, pave the build onto the device.
	logger.Infof(ctx, "paved zedboot, waiting for the device to boot into zedboot")
	listeningName, err = c.deviceResolver.WaitToFindDeviceInNetboot(ctx)
	if err != nil {
		return fmt.Errorf("failed to wait for device to reboot into zedboot: %w", err)
	}

	if err = p.PaveWithOptions(ctx, listeningName, paver.Options{Mode: paver.SkipZedboot}); err != nil {
		return fmt.Errorf("device failed to pave: %w", err)
	}

	logger.Infof(ctx, "paver completed, waiting for device to boot")

	return nil
}

// Flash the device to the specified build. Does not reconnect to the device.
func (c *Client) Flash(
	ctx context.Context,
	ffx *ffx.FFXTool,
	build artifacts.Build,
	publicKey ssh.PublicKey,
) error {
	flasher := ffx.Flasher()
	flasher.SetSSHPublicKey(publicKey)
	flasher.SetTarget(c.Name())

	if productBundleDir, err := build.GetProductBundleDir(ctx); err == nil {
		logger.Infof(ctx, "Flashing with the product bundle %s", productBundleDir)

		flasher.SetProductBundle(productBundleDir)
	} else {
		logger.Warningf(ctx, "Failed to download the product bundle, trying to fall back to the flash manifest: %v", err)

		manifest, err := build.GetFlashManifest(ctx)
		if err != nil {
			return fmt.Errorf("failed to get flash manifest from build: %w", err)
		}

		logger.Infof(ctx, "Flashing with the flash manifest %s", manifest)
		flasher.SetManifest(manifest)
	}

	var err error

	// FIXME(https://fxbug.dev/326658880): We can remove this retry logic after the next stepping stone.
	for i := 0; i < 3; i++ {
		logger.Infof(ctx, "sleeping for 12s before flashing device on attempt %d", i+1)
		time.Sleep(12 * time.Second)
		if _, err = flasher.Flash(ctx); err == nil {
			break
		}
		logger.Infof(ctx, "failed to flash device: %v", err)
	}
	if err != nil {
		return fmt.Errorf("device failed to flash after 3 attempts: %w", err)
	}

	logger.Infof(ctx, "flasher completed, waiting for device to boot")

	return nil
}

func (c *Client) Name() string {
	return c.deviceResolver.NodeName()
}

type addrResolver struct {
	deviceResolver DeviceResolver
}

func (r addrResolver) Resolve(ctx context.Context) (net.Addr, error) {
	addr, err := r.deviceResolver.ResolveSshAddress(ctx)
	if err != nil {
		logger.Warningf(ctx, "failed to resolve ssh address for %v: %v", r.deviceResolver.NodeName(), err)
		return nil, err
	}

	tcpAddr, err := net.ResolveTCPAddr("tcp", addr)
	if err != nil {
		logger.Warningf(ctx, "failed to connect to %v (%v): %v", r.deviceResolver.NodeName(), addr, err)
		return nil, err
	}

	return tcpAddr, nil
}
