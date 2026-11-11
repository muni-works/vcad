# MCP Phase C — live check transcript (2026-11-11)

Real MCP client (`scripts/mcp-phase-c-demo.py`, 2026-07-28 protocol, Tasks
extension) against a vcad.app built from this branch (`apple/VcadApp/bundle.sh`),
launched with an empty `VCAD_DATA_DIR`.

## 1. Long FEA as an MCP Task, cancelled from the app's pill

The agent runs `analyze_structure` (4 convergence levels) as a Task. The pill
side (`vcad/jobs/list` + `vcad/jobs/cancel`, the exact calls the ✕ makes)
sees the job with stage and fraction, then cancels it:

```
task task_490fe411821e93dd: working
  working: running analyze_structure
  cancelled: cancelled by client

pill shows: {"fraction": 0.0003418803418803419, "jobId": "task_490fe411821e93dd", "stage": "Solving level 1 of 4", "state": "running", "taskId": "task_490fe411821e93dd", "tool": "analyze_structure"}
pill shows: [{"fraction": 0.0044444444444444444, "jobId": "task_490fe411821e93dd", "stage": "Solving level 2 of 4", "state": "running", "taskId": "task_490fe411821e93dd", "tool": "analyze_structure"}]
✕ → {'cancelled': 'task_490fe411821e93dd'}
```

## 2. The agent cancels instead (tasks/cancel → job ends in the pill)

```
task task_92e5724deef63ae9: working
  working: running analyze_structure
  working: Solving level 2 of 4 (0.4444444444444444/100)
  agent → tasks/cancel
  cancelled: cancelled by client
```

(After this run the Task status shows a percent, e.g. `Solving level 2 of 4 (0%)`, instead of `(0.44/100)`.)

## 3. Clearance receipt: Holds → Violated after an edit, as structured output

`scene` asserts a 2 mm gap between blocks 6 mm apart (`Holds`, measured 6.0);
`violate` moves B and calls `verify_receipts`:

```
── nudge B  isError=False
── verify_receipts  isError=False
      "measured_mm": 0.0,
      "reason": "\"A\" and \"B\" interpenetrate",
      "status": "Violated",
  "status": "Violated"
```

## Not captured

- **Screenshots**: computer-use access to vcad was declined in this session,
  and vcad's own `app_screenshot`/`snapshot` returns a blank frame for a window
  launched in the background. The pill, the badges and the elicitation banner
  were not photographed.
- **Edge elicitation answered by a click**: `pick_in_viewport` blocks waiting
  for a real click in the viewport, and no click could be sent. The MRTR path
  (`inputResponses`) is covered by the core's conformance tests.
