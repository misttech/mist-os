// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package flasher

import (
	"context"
	"io"
	"os"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"golang.org/x/crypto/ssh"
)

type FfxFlasher struct {
	ffx              *ffx.FFXTool
	flashManifest    string
	useProductBundle bool
	sshPublicKey     ssh.PublicKey
	target           string
}

type Flasher interface {
	Flash(ctx context.Context) error
	SetTarget(ctx context.Context, target string) error
}

// NewFfxFlasher constructs a new flasher that uses `ffx` as the FFXTool used
// to flash a device using flash.json located at `flashManifest`. Also accepts a
// number of optional parameters.
func NewFfxFlasher(ffx *ffx.FFXTool, flashManifest string, useProductBundle bool, options ...FfxFlasherOption) (*FfxFlasher, error) {
	p := &FfxFlasher{
		ffx:              ffx,
		flashManifest:    flashManifest,
		useProductBundle: useProductBundle,
	}

	for _, opt := range options {
		if err := opt(p); err != nil {
			return nil, err
		}
	}

	return p, nil
}

type FfxFlasherOption func(p *FfxFlasher) error

// Sets the SSH public key that the Flasher will bake into the device as an
// authorized key.
func SSHPublicKey(publicKey ssh.PublicKey) FfxFlasherOption {
	return func(p *FfxFlasher) error {
		p.sshPublicKey = publicKey
		return nil
	}
}

// Send stdout from the ffx target flash scripts to `writer`. Defaults to the parent
// stdout.
func Stdout(writer io.Writer) FfxFlasherOption {
	return func(p *FfxFlasher) error {
		p.ffx.SetStdout(writer)
		return nil
	}
}

// SetTarget sets the target to flash.
func (p *FfxFlasher) SetTarget(ctx context.Context, target string) error {
	p.target = target
	return p.ffx.TargetAdd(ctx, target)
}

// Flash a device with flash.json manifest.
func (p *FfxFlasher) Flash(ctx context.Context) error {
	flasherArgs := []string{}

	// Write out the public key's authorized keys.
	if p.sshPublicKey != nil {
		authorizedKeys, err := os.CreateTemp("", "")
		if err != nil {
			return err
		}
		defer os.Remove(authorizedKeys.Name())

		if _, err := authorizedKeys.Write(ssh.MarshalAuthorizedKey(p.sshPublicKey)); err != nil {
			return err
		}

		if err := authorizedKeys.Close(); err != nil {
			return err
		}

		flasherArgs = append(flasherArgs, "--authorized-keys", authorizedKeys.Name())
	}
	if p.useProductBundle {
		flasherArgs = append(flasherArgs, "--product-bundle")
	}
	return p.ffx.Flash(ctx, p.target, p.flashManifest, flasherArgs...)
}
