// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package device

import (
	"context"
	"fmt"
	"net"
	"strconv"
	"time"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type DeviceResolver interface {
	// NodeName returns a nodename for a device.
	NodeName() string

	// Resolve the device's nodename into an ssh address.
	ResolveSshAddress(ctx context.Context) (string, error)

	// Block until the device appears to be in fastboot.
	WaitToFindDeviceInFastboot(ctx context.Context) (string, error)

	// Block until the device appears to be in netboot.
	WaitToFindDeviceInNetboot(ctx context.Context) (string, error)
}

// ConstnatAddressResolver returns a fixed hostname for the specified nodename.
type ConstantHostResolver struct {
	nodeName string
	host     string
	sshPort  int
}

// NewConstantAddressResolver constructs a fixed host.
func NewConstantHostResolver(
	ctx context.Context,
	nodeName string,
	host string,
	sshPort int,
) ConstantHostResolver {
	return ConstantHostResolver{
		nodeName: nodeName,
		host:     host,
		sshPort:  sshPort,
	}
}

func (r ConstantHostResolver) NodeName() string {
	return r.nodeName
}

func (r ConstantHostResolver) ResolveSshAddress(ctx context.Context) (string, error) {
	return net.JoinHostPort(r.host, strconv.Itoa(r.sshPort)), nil
}

func (r ConstantHostResolver) WaitToFindDeviceInFastboot(ctx context.Context) (string, error) {
	// We have no way to tell if the device is in fastboot, so just exit.
	logger.Warningf(ctx, "ConstantHostResolver cannot tell if device is in fastboot, assuming nodename is %s", r.nodeName)
	return r.nodeName, nil
}

func (r ConstantHostResolver) WaitToFindDeviceInNetboot(ctx context.Context) (string, error) {
	// We have no way to tell if the device is in netboot, so just exit.
	logger.Warningf(ctx, "ConstantHostResolver cannot tell if device is in netboot, assuming nodename is %s", r.nodeName)
	return r.nodeName, nil
}

// FfxResolver uses `ffx target list` to resolve a nodename into a hostname.
type FfxResolver struct {
	ffx         *ffx.FFXTool
	nodeName    string
	oldNodeName string
}

// NewFffResolver constructs a new `FfxResolver` for the specific nodename.
func NewFfxResolver(
	ctx context.Context,
	ffxTool *ffx.FFXTool,
	nodeName string,
) (*FfxResolver, error) {
	if nodeName == "" {
		var entries []ffx.TargetEntry
		var err error
		for i := 0; i < 10; i++ {
			entries, err = ffxTool.TargetList(ctx)

			if err != nil {
				return nil, fmt.Errorf("failed to list devices: %w", err)
			}

			if len(entries) > 0 {
				break
			}

			logger.Infof(ctx, "no devices found, retrying")
			time.Sleep(5 * time.Second)
		}

		if len(entries) == 0 {
			return nil, fmt.Errorf("no devices found")
		}

		if len(entries) != 1 {
			return nil, fmt.Errorf("cannot use empty nodename with multiple devices: %v", entries)
		}

		nodeName = entries[0].NodeName
	}

	return &FfxResolver{
		ffx:      ffxTool,
		nodeName: nodeName,
	}, nil
}

func (r *FfxResolver) NodeName() string {
	return r.nodeName
}

func (r *FfxResolver) ResolveName(ctx context.Context) (string, error) {
	nodeName := r.NodeName()
	logger.Infof(ctx, "resolving the nodename %v hostname", nodeName)

	targets, err := r.ffx.TargetListForNode(ctx, nodeName)
	if err != nil {
		return "", err
	}

	logger.Infof(ctx, "resolved the nodename %v to %v", nodeName, targets)

	if len(targets) == 0 {
		return "", fmt.Errorf("no addresses found for nodename: %v", nodeName)
	}

	if len(targets) > 1 {
		return "", fmt.Errorf("multiple addresses found for nodename %v: %v", nodeName, targets)
	}

	target := targets[0]

	if len(target.Addresses) == 0 {
		return "", fmt.Errorf("no address found for nodename %v: %v", nodeName, target)
	}

	return target.Addresses[0], nil
}

func (r *FfxResolver) ResolveSshAddress(ctx context.Context) (string, error) {
	nodeName := r.NodeName()
	logger.Infof(ctx, "resolving the nodename %v ssh address", nodeName)

	addr, err := r.ffx.TargetGetSshAddress(ctx, nodeName)
	if err != nil {
		return "", fmt.Errorf("no ssh address found for nodename %v: %w", nodeName, err)
	}

	return addr, nil
}

func (r *FfxResolver) WaitToFindDeviceInFastboot(ctx context.Context) (string, error) {
	nodeName := r.NodeName()

	// Wait for the device to be listening in netboot.
	logger.Infof(ctx, "waiting for the device to be listening on the nodename: %v", nodeName)

	attempt := 0
	for {
		attempt += 1

		if entries, err := r.ffx.TargetListForNode(ctx, nodeName); err == nil {
			for _, entry := range entries {
				logger.Infof(ctx, "device is in %v", entry.TargetState)
				if entry.TargetState == "Fastboot" {
					logger.Infof(ctx, "device %v is listening on %q", entry.NodeName, entry)
					return entry.NodeName, nil
				}
			}

			logger.Infof(ctx, "attempt %d waiting for device to boot into fastboot", attempt)
			time.Sleep(5 * time.Second)
		} else {
			logger.Infof(ctx, "attempt %d failed to resolve nodename %v: %v", attempt, nodeName, err)
		}

	}
}

func (r *FfxResolver) WaitToFindDeviceInNetboot(ctx context.Context) (string, error) {
	// Exit early if ffx is not configured to listen for devices in zedboot.
	supportsZedbootDiscovery, err := r.ffx.SupportsZedbootDiscovery(ctx)
	if err != nil {
		return "", err
	}

	if !supportsZedbootDiscovery {
		logger.Warningf(ctx, "ffx not configured to listen for devices in zedboot, assuming nodename is %s", r.nodeName)
		return r.nodeName, nil
	}

	nodeName := r.NodeName()

	// Wait for the device to be listening in netboot.
	logger.Infof(ctx, "waiting for the to be listening on the nodename: %v", nodeName)

	attempt := 0
	for {
		attempt += 1

		if entries, err := r.ffx.TargetListForNode(ctx, nodeName); err == nil {
			for _, entry := range entries {
				logger.Infof(ctx, "device is in %v", entry.TargetState)
				if entry.TargetState == "Zedboot (R)" {
					logger.Infof(ctx, "device %v is listening on %q", entry.NodeName, entry)
					return entry.NodeName, nil
				}
			}

			logger.Infof(ctx, "attempt %d waiting for device to boot into zedboot", attempt)
			time.Sleep(5 * time.Second)
		} else {
			logger.Infof(ctx, "attempt %d failed to resolve nodename %v: %v", attempt, nodeName, err)
		}

	}
}
