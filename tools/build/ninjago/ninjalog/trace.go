// Copyright 2014 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ninjalog

import (
	"sort"
	"time"

	"go.fuchsia.dev/fuchsia/tools/build/ninjago/chrometrace"
)

func toTrace(step Step, pid int, tid int) chrometrace.Trace {
	tr := chrometrace.Trace{
		Name:            step.Out,
		Category:        step.Category(),
		EventType:       chrometrace.CompleteEvent,
		TimestampMicros: int(step.Start / time.Microsecond),
		DurationMicros:  int(step.Duration() / time.Microsecond),
		ProcessID:       pid,
		ThreadID:        tid,
		Args: map[string]interface{}{
			"outputs":     step.AllOutputs(),
			"total float": step.TotalFloat.String(),
		},
	}

	if step.Command != nil {
		tr.Args["command"] = step.Command.Command
	}
	if step.OnCriticalPath {
		// Add "critical_path" to category to allow easy searching and highlighting
		// in tracer viewer.
		//
		// Note: given chrome trace viewer does a full text search, searching
		// "critical_path" might yield false positives if they happen to have this
		// string in their output name or command. If we see this happening often we
		// can make this string more unique.
		tr.Category += ",critical_path"
		tr.Args["drag"] = step.Drag.String()
	}
	return tr
}

func toFlowEvents(from, to Step, id int, pid int, tids map[string]int) []chrometrace.Trace {
	return []chrometrace.Trace{
		{
			Name:      "critical_path",
			Category:  "critical_path",
			EventType: chrometrace.FlowEventStart,
			// Start flow event arrow from the middle of the "from" event.
			TimestampMicros: int((from.Start + from.Duration()/2) / time.Microsecond),
			ProcessID:       pid,
			ThreadID:        tids[from.Out],
			ID:              id,
		},
		{
			Name:      "critical_path",
			Category:  "critical_path",
			EventType: chrometrace.FlowEventEnd,
			// Point flow event arrow to the middle of the "to" event.
			TimestampMicros: int((to.Start + to.Duration()/2) / time.Microsecond),
			ProcessID:       pid,
			ThreadID:        tids[to.Out],
			ID:              id,
			// Set "enclosing slice" as the binding point. This tells the trace viewer
			// `TimestampMicros` is in the middle of the "to" event.
			//
			// https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/edit#heading=h.4qqub5rv9ybk
			BindingPoint: "e",
		},
	}
}

// ToTraces converts Flow outputs into trace log.
//
// If a non-empty `criticalPath` is provided, steps on the critical path will
// have "critical_path" in their category to enable quick searching and
// highlighting.
func ToTraces(steps [][]Step, pid int) []chrometrace.Trace {
	var criticalThreads, nonCriticalThreads [][]Step
Outer:
	for _, thread := range steps {
		// Elevate threads with critical steps to the top of the trace.
		for _, s := range thread {
			if s.OnCriticalPath {
				criticalThreads = append(criticalThreads, thread)
				continue Outer
			}
		}
		nonCriticalThreads = append(nonCriticalThreads, thread)
	}
	steps = append(criticalThreads, nonCriticalThreads...)

	var criticalPath []Step
	var traces []chrometrace.Trace
	// Record `tid`s of all critical steps for generating flow events later.
	tids := make(map[string]int)
	for tid, thread := range steps {
		for _, step := range thread {
			traces = append(traces, toTrace(step, pid, tid))
			if step.OnCriticalPath {
				tids[step.Out] = tid
				criticalPath = append(criticalPath, step)
			}
		}
	}

	sort.SliceStable(criticalPath, func(i, j int) bool { return criticalPath[i].Start < criticalPath[j].Start })
	// Draw lines (flow events) between all critical steps.
	for i := 1; i < len(criticalPath); i++ {
		traces = append(traces, toFlowEvents(criticalPath[i-1], criticalPath[i], i, pid, tids)...)
	}

	sort.Stable(chrometrace.ByStart(traces))
	return traces
}
