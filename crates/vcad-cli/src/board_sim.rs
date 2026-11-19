//! `vcad board-sim`: boot a board's pinned gateware in Verilator with every
//! board net resolved pin by pin (see `vcad_ecad_sim::board_sim`).

use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

use vcad_ecad_schematic::drive::Pull;
use vcad_ecad_sim::board_sim::{
    find_fpga_ref, run, uart::Console, uart::Ft231x, verilated::BuildOptions,
    verilated::VerilatedFpga, Board, Milestones, PinModel, StopReason, LINUX_MILESTONES,
};
use vcad_ecad_sim::gateware::{ArtifactStore, ULX3S_UART_LINUX};
use vcad_ir::ecad::SchematicSheet;

pub struct BoardSimArgs {
    pub board: PathBuf,
    pub artifact: Option<String>,
    pub tarball: Option<PathBuf>,
    pub fpga_ref: Option<String>,
    pub max_cycles: u64,
    pub until: Option<String>,
    pub pulls: Vec<String>,
    pub erc_only: bool,
    pub quiet: bool,
}

/// Runs the sim. Returns whether the run met its goal: `--until` reached,
/// or (without `--until`) the cycle budget completed.
pub fn run_board_sim(a: BoardSimArgs) -> Result<bool> {
    let text =
        std::fs::read_to_string(&a.board).with_context(|| format!("read {}", a.board.display()))?;
    let sheet: SchematicSheet = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a schematic sheet", a.board.display()))?;

    let store = ArtifactStore::from_env();
    let art = match &a.tarball {
        Some(t) => store.import_tarball(t, a.artifact.as_deref())?,
        None => store.resolve(a.artifact.as_deref().unwrap_or(ULX3S_UART_LINUX))?,
    };
    let m = &art.manifest;
    eprintln!(
        "[board-sim] gateware {} ({}), cpu {}…, clock {} Hz",
        m.name,
        art.root.display(),
        &m.cpu.sha256[..12],
        m.clock.hz
    );

    let mut pulls = BTreeMap::new();
    for p in &a.pulls {
        let (net, dir) = p
            .split_once('=')
            .ok_or_else(|| anyhow!("--pull wants NET=up|down, got {p:?}"))?;
        let dir = match dir {
            "up" => Pull::Up,
            "down" => Pull::Down,
            _ => bail!("--pull wants NET=up|down, got {p:?}"),
        };
        pulls.insert(net.to_string(), dir);
    }

    let fpga_ref = find_fpga_ref(&sheet, m, a.fpga_ref.as_deref())?;
    let uart = m
        .uart
        .clone()
        .ok_or_else(|| anyhow!("gateware {} has no console UART", m.name))?;
    let console = Console::default();
    let mut models: Vec<(String, Box<dyn PinModel>)> = Vec::new();
    for c in &sheet.components {
        if c.value.starts_with("FT231X") {
            models.push((
                c.reference.clone(),
                Box::new(Ft231x::new(
                    m.clock.hz,
                    uart.baud,
                    console.clone(),
                    !a.quiet,
                )),
            ));
        }
    }
    if models.is_empty() {
        bail!("no FT231X on the board: nothing would hear the console");
    }
    let mut board = Board::new(&sheet, m, &fpga_ref, models, pulls)?;

    eprintln!("[board-sim] FPGA {fpga_ref}; gateware pins:");
    for p in &m.pins {
        eprintln!("[board-sim]   {}@{} ({:?})", p.signal, p.ball, p.dir);
    }
    if board.erc.is_empty() {
        eprintln!("ERC: clean");
    }
    for v in &board.erc {
        eprintln!("ERC: {:?}: {}", v.severity, v.message);
    }
    for d in &board.unmodelled_drivers {
        eprintln!("[board-sim] warning: no model for driver {d}; its net reads the pull level");
    }
    if a.erc_only {
        return Ok(board.erc.is_empty());
    }

    let mut fpga = VerilatedFpga::new(&art, &BuildOptions::default())?;
    let mut ms = Milestones::new(LINUX_MILESTONES);
    let until = a.until.as_deref();
    if let Some(u) = until {
        if !LINUX_MILESTONES.iter().any(|(n, _)| *n == u) {
            let names: Vec<&str> = LINUX_MILESTONES.iter().map(|(n, _)| *n).collect();
            bail!("--until {u:?}: expected one of {}", names.join(", "));
        }
    }
    eprintln!(
        "[board-sim] running up to {} cycles{}",
        a.max_cycles,
        until.map(|u| format!(" or until {u}")).unwrap_or_default()
    );
    let out = run(
        &mut board,
        &mut fpga,
        &console,
        &mut ms,
        a.max_cycles,
        until,
        |name, cycle, secs| {
            eprintln!("\n### MARK {name} cycle={cycle} wall={secs:.1}s");
        },
    );
    eprintln!(
        "\n[board-sim] {} cycles in {:.1}s ({:.0} cycles/s), console bytes: {}, stop: {:?}",
        out.cycles,
        out.secs,
        out.cycles as f64 / out.secs.max(1e-9),
        out.console_bytes,
        out.stop
    );
    let marks: Vec<String> = ms
        .reached
        .iter()
        .map(|(n, c, s)| format!("{n}@{c}({s:.0}s)"))
        .collect();
    eprintln!("[board-sim] milestones: {}", marks.join(" "));
    Ok(match until {
        Some(_) => out.stop == StopReason::Reached,
        None => true,
    })
}
