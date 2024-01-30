// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Package utils provides shared utilities needed within orchestrate.
package orchestrate

import (
	"context"
	"time"
)

// RunWithRetries invokes `callable` retrying up to `maxRetries` and delaying by
// `retryDelay` each time there's an error.
func RunWithRetries(ctx context.Context, retryDelay time.Duration, maxRetries int, callable func() error) error {
	currAttempt := 0
	for {
		if err := callable(); err == nil || currAttempt == maxRetries {
			return err
		}
		currAttempt++

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(retryDelay):
		}
	}
}
