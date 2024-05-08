// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package cli

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"

	"golang.org/x/crypto/ssh"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/device"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/tools/botanist/constants"
	"go.fuchsia.dev/fuchsia/tools/lib/retry"
)

type DeviceResolverMode = string

const (
	// Resolve devices with ffx.
	FfxResolver = "ffx"
)

type DeviceConfig struct {
	sshKeyFile               string
	deviceFinderPath         string
	deviceName               string
	deviceHostname           string
	deviceResolverMode       DeviceResolverMode
	deviceSshPort            int
	repoPort                 int
	sshPrivateKey            ssh.Signer
	SerialSocketPath         string
	connectTimeout           time.Duration
	WorkaroundBrokenTimeSkip bool
	testDataPath             string
}

func NewDeviceConfig(fs *flag.FlagSet, testDataPath string) *DeviceConfig {
	c := &DeviceConfig{}
	c.testDataPath = testDataPath
	fs.StringVar(&c.sshKeyFile, "ssh-private-key", os.Getenv(constants.SSHKeyEnvKey), "SSH private key file that can access the device")
	fs.StringVar(&c.deviceName, "device", os.Getenv(constants.NodenameEnvKey), "device name")
	fs.StringVar(&c.deviceHostname, "device-hostname", os.Getenv(constants.DeviceAddrEnvKey), "device hostname or IPv4/IPv6 address")
	fs.StringVar(&c.deviceResolverMode, "device-resolver", FfxResolver, "device resolver (default: ffx)")
	fs.IntVar(&c.deviceSshPort, "device-ssh-port", 22, "device port")
	fs.StringVar(&c.deviceFinderPath, "device-finder-path", "", "device-finder tool path")
	fs.StringVar(&c.SerialSocketPath, "device-serial", "", "device serial path")
	fs.DurationVar(&c.connectTimeout, "device-connect-timeout", 5*time.Second, "device connection timeout (default 5 seconds)")
	fs.BoolVar(&c.WorkaroundBrokenTimeSkip, "workaround-broken-time-skip", false,
		"whether to sleep for 15 seconds after pave and then reconnect, to work around a known networking bug, https://fxbug.dev/42154590")
	fs.IntVar(&c.repoPort, "repo-port", 0, "default port to serve the repository")

	environmentSerialPath := os.Getenv(constants.SerialSocketEnvKey)
	if c.SerialSocketPath == "" && environmentSerialPath != "" {
		c.SerialSocketPath = environmentSerialPath
	}

	return c
}

func (c *DeviceConfig) Validate() error {
	for _, s := range []string{
		c.sshKeyFile,
		c.SerialSocketPath,
	} {
		if err := util.ValidatePath(s); err != nil {
			return err
		}
	}
	return nil
}

func (c *DeviceConfig) deviceResolver(
	ctx context.Context,
	ffx *ffx.FFXTool,
) (device.DeviceResolver, error) {
	if c.deviceHostname != "" {
		return device.NewConstantHostResolver(
			ctx,
			c.deviceName,
			c.deviceHostname,
			c.deviceSshPort,
		), nil
	}

	switch c.deviceResolverMode {
	case FfxResolver:
		return device.NewFfxResolver(ctx, ffx, c.deviceName)

	default:
		return nil, fmt.Errorf("Invalid device-resolver mode %v", c.deviceResolverMode)
	}
}

func (c *DeviceConfig) SSHPrivateKey() (ssh.Signer, error) {
	if c.sshPrivateKey == nil {
		if c.sshKeyFile == "" {
			return nil, fmt.Errorf("ssh private key cannot be empty")
		}

		key, err := os.ReadFile(c.sshKeyFile)
		if err != nil {
			return nil, err
		}

		privateKey, err := ssh.ParsePrivateKey(key)
		if err != nil {
			return nil, err
		}
		c.sshPrivateKey = privateKey
	}

	return c.sshPrivateKey, nil
}

func (c *DeviceConfig) NewDeviceClient(
	ctx context.Context,
	ffx *ffx.FFXTool,
) (*device.Client, error) {
	deviceResolver, err := c.deviceResolver(ctx, ffx)
	if err != nil {
		return nil, err
	}

	sshPrivateKey, err := c.SSHPrivateKey()
	if err != nil {
		return nil, err
	}

	connectBackoff := retry.NewConstantBackoff(c.connectTimeout)

	var serialConn *device.SerialConn
	if c.SerialSocketPath != "" {
		serialConn, err = device.NewSerialConn(c.SerialSocketPath)
		if err != nil {
			return nil, err
		}
	}

	client, err := device.NewClient(
		ctx,
		c.repoPort,
		deviceResolver,
		sshPrivateKey,
		connectBackoff,
		c.WorkaroundBrokenTimeSkip,
		serialConn,
	)
	if err != nil {
		return nil, err
	}

	return client, nil
}
