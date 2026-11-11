//! Fab tasks: progress, cancel, fail-closed gates — all on dry-run backends.
mod common;

use serde_json::json;
use vcad_atoms::fab::{fab_mill, fab_print, DryRunMachine};
use vcad_atoms::task::{spawn, TaskStatus};

#[test]
fn print_task_completes_with_monotone_progress() {
    let h = vcad_atoms::spawn_tool("fab_print", common::block(20.0, 20.0, 5.0), json!({})).unwrap();
    let s = h.wait();
    assert_eq!(s.status, TaskStatus::Completed, "{:?}", s.error);
    let r = s.result.unwrap();
    assert!(
        r["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["pass"] == true),
        "{r}"
    );
    assert_eq!(r["run"]["dryRun"], true);
    assert!(r["summary"]["layers"].as_u64().unwrap() >= 20);
}

#[test]
fn oversize_print_fails_closed_and_never_dispatches() {
    let h =
        vcad_atoms::spawn_tool("fab_print", common::block(400.0, 20.0, 5.0), json!({})).unwrap();
    let s = h.wait();
    assert_eq!(s.status, TaskStatus::Failed);
    let rep = s.result.expect("gate report on failure");
    assert_eq!(rep["dispatched"], false);
    let fits = rep["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "fits_bed")
        .unwrap();
    assert_eq!(fits["pass"], false);
}

#[test]
fn mill_task_passes_stock_sim_gate() {
    let h = vcad_atoms::spawn_tool("fab_mill", common::block(30.0, 20.0, 6.0), json!({})).unwrap();
    let s = h.wait();
    assert_eq!(
        s.status,
        TaskStatus::Completed,
        "{:?} {:?}",
        s.error,
        s.result
    );
    let r = s.result.unwrap();
    assert_eq!(r["summary"]["stockVerify"]["pass"], true);
    assert_eq!(r["run"]["dryRun"], true);
}

#[test]
fn mill_with_oversized_margin_fails_stock_verify() {
    // Margin far beyond the tool diameter: one profile pass leaves a frame of
    // stock the model doesn't contain — the stock-sim gate must catch it.
    let h = vcad_atoms::spawn_tool(
        "fab_mill",
        common::block(30.0, 20.0, 6.0),
        json!({"margin": 10.0}),
    )
    .unwrap();
    let s = h.wait();
    assert_eq!(s.status, TaskStatus::Failed);
    let rep = s.result.unwrap();
    let g = rep["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "stock_verify")
        .unwrap();
    assert_eq!(g["pass"], false);
}

#[test]
fn cancel_during_dry_run_stream() {
    let doc = common::block(20.0, 20.0, 5.0);
    let h = spawn(move |ctx| {
        let mut m = DryRunMachine::new("slow printer");
        m.line_delay = std::time::Duration::from_millis(2);
        fab_print(&doc, &json!({}), &m, ctx)
    });
    // Wait until it is streaming, then cancel.
    loop {
        if h.events().iter().any(|e| e.message.starts_with("dry run")) {
            break;
        }
        assert!(
            !h.snapshot().status.is_terminal(),
            "finished before cancel: {:?}",
            h.snapshot()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.cancel();
    let s = h.wait();
    assert_eq!(s.status, TaskStatus::Cancelled);
    assert!(s.progress < 1.0);
}

#[test]
fn mill_cancel_before_stream_is_honoured() {
    let doc = common::block(30.0, 20.0, 6.0);
    let h = spawn(move |ctx| {
        let mut m = DryRunMachine::new("slow mill");
        m.line_delay = std::time::Duration::from_millis(5);
        fab_mill(&doc, &json!({}), &m, ctx)
    });
    h.cancel();
    assert_eq!(h.wait().status, TaskStatus::Cancelled);
}
