// Copyright 2014 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ninjalog

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"go.fuchsia.dev/fuchsia/tools/build/ninjago/chrometrace"
	"go.fuchsia.dev/fuchsia/tools/build/ninjago/compdb"
)

func TestToTraces(t *testing.T) {
	for _, tc := range []struct {
		desc         string
		flow         [][]Step
		criticalPath []Step
		want         []chrometrace.Trace
	}{
		{
			desc: "empty",
		},
		{
			desc: "without critical path",
			flow: [][]Step{
				{
					{
						Start:   76 * time.Millisecond,
						End:     187 * time.Millisecond,
						Out:     "resources/inspector/devtools_extension_api.js",
						CmdHash: 0x75430546595be7c2,
					},
					{
						Start:      187 * time.Millisecond,
						End:        21304 * time.Millisecond,
						Out:        "obj/third_party/pdfium/core/src/fpdfdoc/fpdfdoc.doc_formfield.o",
						CmdHash:    0x2ac7111aa1ae86af,
						TotalFloat: 100 * time.Millisecond,
						Command: &compdb.Command{
							Command: "prebuilt/third_party/clang/linux-x64/clang++ some args and files",
						},
					},
				},
				{
					{
						Start:   78 * time.Millisecond,
						End:     286 * time.Millisecond,
						Out:     "gen/angle/commit_id.py",
						CmdHash: 0x4ede38e2c1617d8c,
					},
					{
						Start:      287 * time.Millisecond,
						End:        290 * time.Millisecond,
						Out:        "obj/third_party/angle/src/copy_scripts.actions_rules_copies.stamp",
						CmdHash:    0xb211d373de72f455,
						TotalFloat: 420 * time.Millisecond,
						Command: &compdb.Command{
							Command: "touch obj/third_party/angle/src/copy_scripts.actions_rules_copies.stamp",
						},
					},
				},
				{
					{
						Start:   79 * time.Millisecond,
						End:     287 * time.Millisecond,
						Out:     "gen/angle/copy_compiler_dll.bat",
						CmdHash: 0x9fb635ad5d2c1109,
					},
				},
				{
					{
						Start:   80 * time.Millisecond,
						End:     284 * time.Millisecond,
						Out:     "gen/autofill_regex_constants.cc",
						CmdHash: 0xfa33c8d7ce1d8791,
					},
				},
				{
					{
						Start:   141 * time.Millisecond,
						End:     287 * time.Millisecond,
						Out:     "PepperFlash/manifest.json",
						CmdHash: 0x324f0a0b77c37ef,
					},
				},
				{
					{
						Start:   142 * time.Millisecond,
						End:     288 * time.Millisecond,
						Out:     "PepperFlash/libpepflashplayer.so",
						CmdHash: 0x1e2c2b7845a4d4fe,
					},
				},
			},
			want: []chrometrace.Trace{
				{
					Name:            "resources/inspector/devtools_extension_api.js",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 76 * 1000,
					DurationMicros:  (187 - 76) * 1000,
					ProcessID:       1,
					ThreadID:        0,
					Args: map[string]interface{}{
						"outputs":     []string{"resources/inspector/devtools_extension_api.js"},
						"total float": "0s",
					},
				},
				{
					Name:            "gen/angle/commit_id.py",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 78 * 1000,
					DurationMicros:  (286 - 78) * 1000,
					ProcessID:       1,
					ThreadID:        1,
					Args: map[string]interface{}{
						"outputs":     []string{"gen/angle/commit_id.py"},
						"total float": "0s",
					},
				},
				{
					Name:            "gen/angle/copy_compiler_dll.bat",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 79 * 1000,
					DurationMicros:  (287 - 79) * 1000,
					ProcessID:       1,
					ThreadID:        2,
					Args: map[string]interface{}{
						"outputs":     []string{"gen/angle/copy_compiler_dll.bat"},
						"total float": "0s",
					},
				},
				{
					Name:            "gen/autofill_regex_constants.cc",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 80 * 1000,
					DurationMicros:  (284 - 80) * 1000,
					ProcessID:       1,
					ThreadID:        3,
					Args: map[string]interface{}{
						"outputs":     []string{"gen/autofill_regex_constants.cc"},
						"total float": "0s",
					},
				},
				{
					Name:            "PepperFlash/manifest.json",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 141 * 1000,
					DurationMicros:  (287 - 141) * 1000,
					ProcessID:       1,
					ThreadID:        4,
					Args: map[string]interface{}{
						"outputs":     []string{"PepperFlash/manifest.json"},
						"total float": "0s",
					},
				},
				{
					Name:            "PepperFlash/libpepflashplayer.so",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 142 * 1000,
					DurationMicros:  (288 - 142) * 1000,
					ProcessID:       1,
					ThreadID:        5,
					Args: map[string]interface{}{
						"outputs":     []string{"PepperFlash/libpepflashplayer.so"},
						"total float": "0s",
					},
				},
				{
					Name:            "obj/third_party/pdfium/core/src/fpdfdoc/fpdfdoc.doc_formfield.o",
					Category:        "clang++",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 187 * 1000,
					DurationMicros:  (21304 - 187) * 1000,
					ProcessID:       1,
					ThreadID:        0,
					Args: map[string]interface{}{
						"outputs":     []string{"obj/third_party/pdfium/core/src/fpdfdoc/fpdfdoc.doc_formfield.o"},
						"command":     "prebuilt/third_party/clang/linux-x64/clang++ some args and files",
						"total float": "100ms",
					},
				},
				{
					Name:            "obj/third_party/angle/src/copy_scripts.actions_rules_copies.stamp",
					Category:        "touch",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 287 * 1000,
					DurationMicros:  (290 - 287) * 1000,
					ProcessID:       1,
					ThreadID:        1,
					Args: map[string]interface{}{
						"outputs":     []string{"obj/third_party/angle/src/copy_scripts.actions_rules_copies.stamp"},
						"command":     "touch obj/third_party/angle/src/copy_scripts.actions_rules_copies.stamp",
						"total float": "420ms",
					},
				},
			},
		},
		{
			desc: "with critical path",
			flow: [][]Step{
				{
					{
						End:        5 * time.Microsecond,
						Out:        "a",
						Outs:       []string{"foo", "bar"},
						TotalFloat: 6789 * time.Microsecond,
					},
				},
				{
					{
						Start:      1 * time.Microsecond,
						End:        2 * time.Microsecond,
						Out:        "b",
						TotalFloat: 9876 * time.Microsecond,
					},
					{
						Start:          10 * time.Microsecond,
						End:            20 * time.Microsecond,
						Drag:           1234 * time.Microsecond,
						Out:            "critical_c",
						OnCriticalPath: true,
					},
				},
				{
					{
						Start:          3 * time.Microsecond,
						End:            5 * time.Microsecond,
						Out:            "critical_a",
						Outs:           []string{"baz"},
						OnCriticalPath: true,
						Drag:           4321 * time.Microsecond,
					},
					{
						Start:          5 * time.Microsecond,
						End:            10 * time.Microsecond,
						Out:            "critical_b",
						OnCriticalPath: true,
						Drag:           2345 * time.Microsecond,
					},
				},
			},
			want: []chrometrace.Trace{
				{
					Name:            "a",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 0,
					DurationMicros:  5,
					ProcessID:       1,
					ThreadID:        2,
					Args: map[string]interface{}{
						"outputs":     []string{"foo", "bar", "a"},
						"total float": "6.789ms",
					},
				},
				{
					Name:            "b",
					Category:        "unknown",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 1,
					DurationMicros:  1,
					ProcessID:       1,
					ThreadID:        0,
					Args: map[string]interface{}{
						"outputs":     []string{"b"},
						"total float": "9.876ms",
					},
				},

				{
					Name:            "critical_a",
					Category:        "unknown,critical_path",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 3,
					DurationMicros:  2,
					ProcessID:       1,
					ThreadID:        1,
					Args: map[string]interface{}{
						"outputs":     []string{"baz", "critical_a"},
						"drag":        "4.321ms",
						"total float": "0s",
					},
				},
				{
					// Flow event going out of critical_a.
					Name:            "critical_path",
					Category:        "critical_path",
					EventType:       chrometrace.FlowEventStart,
					TimestampMicros: 4,
					ProcessID:       1,
					ThreadID:        1,
					ID:              1,
				},
				{
					Name:            "critical_b",
					Category:        "unknown,critical_path",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 5,
					DurationMicros:  5,
					ProcessID:       1,
					ThreadID:        1,
					Args: map[string]interface{}{
						"outputs":     []string{"critical_b"},
						"drag":        "2.345ms",
						"total float": "0s",
					},
				},
				{
					// Flow event pointing to critical_b.
					Name:            "critical_path",
					Category:        "critical_path",
					EventType:       chrometrace.FlowEventEnd,
					TimestampMicros: 7,
					ProcessID:       1,
					ThreadID:        1,
					ID:              1,
					BindingPoint:    "e",
				},
				{
					// Flow event going out of critical_b.
					Name:            "critical_path",
					Category:        "critical_path",
					EventType:       chrometrace.FlowEventStart,
					TimestampMicros: 7,
					ProcessID:       1,
					ThreadID:        1,
					ID:              2,
				},
				{
					Name:            "critical_c",
					Category:        "unknown,critical_path",
					EventType:       chrometrace.CompleteEvent,
					TimestampMicros: 10,
					DurationMicros:  10,
					ProcessID:       1,
					ThreadID:        0,
					Args: map[string]interface{}{
						"outputs":     []string{"critical_c"},
						"drag":        "1.234ms",
						"total float": "0s",
					},
				},
				{
					// Flow event pointing to critical_c.
					Name:            "critical_path",
					Category:        "critical_path",
					EventType:       chrometrace.FlowEventEnd,
					TimestampMicros: 15,
					ProcessID:       1,
					ThreadID:        0,
					ID:              2,
					BindingPoint:    "e",
				},
			},
		},
	} {
		t.Run(tc.desc, func(t *testing.T) {
			got := ToTraces(tc.flow, 1)
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("ToTrace()=%#v\nwant=%#v\ndiff (-want +got):\n%s", got, tc.want, diff)
			}
		})
	}
}
