//! Pin-level board simulation: gateware in an HDL model, copper in Rust.
//!
//! An FPGA's gateware (a pinned [`crate::gateware`] artifact) exposes its
//! balls as top-level ports. Every other part on the board is a Rust
//! [`PinModel`]. Each sys-clock cycle the [`Board`] resolves every net the
//! way copper does, with the same rules as the static ERC pass
//! ([`vcad_ecad_schematic::drive`]):
//!
//! - exactly one driver: receivers see its level;
//! - no driver: receivers see the declared pull (pull-up when none is
//!   declared, the UART idle level);
//! - two or more drivers: contention, receivers see the pull level.
//!
//! A miswired board therefore fails the way the hardware would (a dead
//! console), not because a checker said so; the ERC report says why.
//!
//! The HDL side is the [`ChipBackend`] trait. With the `hdl` feature,
//! [`verilated::VerilatedFpga`] builds the artifact's Verilog with
//! Verilator into a cached shared library and drives it over a C ABI.

pub mod uart;
#[cfg(feature = "hdl")]
pub mod verilated;

use std::collections::{BTreeMap, HashMap};
use vcad_ecad_schematic::drive::{
    net_drive_violations, resolve_net_drive, DirectionOverlay, NetDrive, NetDriveOptions,
    NetDriveStatus, PinDir, Pull,
};
use vcad_ecad_schematic::erc::ErcViolation;
use vcad_ir::ecad::SchematicSheet;

use crate::gateware::{GatewareDir, GatewareManifest};

/// The chip side of the board: any backend that exposes a programmable
/// part's balls as indexed pins and advances in sys-clock cycles.
///
/// Verilated gateware ([`verilated::VerilatedFpga`], feature `hdl`) is one
/// implementation. A faster instruction-level or QEMU/Renode-style VM can
/// implement the same trait by mapping its peripheral models (e.g. a UART)
/// onto the pins, and the board, ERC and part models stay unchanged.
///
/// Pin `i` is `manifest.pins[i]`.
pub trait ChipBackend {
    /// Drive an input ball for the next cycle.
    fn set_input(&mut self, pin: usize, level: bool);
    /// Level the gateware drives on an output ball.
    fn output(&self, pin: usize) -> bool;
    /// Advance one sys-clock cycle (rising + falling edge).
    fn cycle(&mut self);
}

/// A behavioural model of a board part, bound to schematic pins by name.
pub trait PinModel {
    /// The model's pins: schematic pin name and direction (`Out`/`Shared`
    /// drive, `In` listens).
    fn pins(&self) -> Vec<(String, PinDir)>;
    /// Level the model drives on its pin `pin` (index into [`Self::pins`]).
    fn drive(&self, pin: usize) -> bool;
    /// Level the copper presents on its input pin `pin`.
    fn sense(&mut self, pin: usize, level: bool);
    /// Advance one sys-clock cycle.
    fn tick(&mut self);
}

/// One end of a simulated net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Fpga(usize),
    Model(usize, usize),
    /// A driver nothing simulates (no model for its part).
    Unmodelled,
}

#[derive(Debug, Clone)]
struct SimNet {
    drivers: Vec<End>,
    receivers: Vec<End>,
    idle: bool,
}

/// Errors building a board.
#[derive(Debug, thiserror::Error)]
pub enum BoardError {
    /// No or several schematic components match the gateware's FPGA.
    #[error("{0}")]
    Fpga(String),
    /// A model's pin name is not on its component.
    #[error("{0}")]
    Model(String),
}

/// A board ready to simulate: the resolved net topology plus the models.
pub struct Board {
    nets: Vec<SimNet>,
    models: Vec<Box<dyn PinModel>>,
    /// FPGA input pins on no net: held at the pull-up.
    floating_inputs: Vec<usize>,
    /// Static drive resolution (the ERC view of the same copper).
    pub drive: Vec<NetDrive>,
    /// Contention / undriven findings.
    pub erc: Vec<ErcViolation>,
    /// Reference designator of the FPGA.
    pub fpga_ref: String,
    /// Drivers on simulated nets that no model covers (`"REF.pin"`): they
    /// count toward contention but their level is unknown.
    pub unmodelled_drivers: Vec<String>,
    cycles: u64,
}

impl std::fmt::Debug for Board {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Board")
            .field("fpga_ref", &self.fpga_ref)
            .field("nets", &self.nets.len())
            .field("models", &self.models.len())
            .field("erc", &self.erc)
            .finish()
    }
}

/// Find the FPGA component: `fpga_ref` if given, else the one component
/// whose value equals the manifest's `fpga_ref_part`.
pub fn find_fpga_ref(
    sheet: &SchematicSheet,
    manifest: &GatewareManifest,
    fpga_ref: Option<&str>,
) -> Result<String, BoardError> {
    if let Some(r) = fpga_ref {
        return if sheet.components.iter().any(|c| c.reference == r) {
            Ok(r.to_string())
        } else {
            Err(BoardError::Fpga(format!("no component {r} on the board")))
        };
    }
    let hits: Vec<&str> = sheet
        .components
        .iter()
        .filter(|c| c.value == manifest.fpga_ref_part)
        .map(|c| c.reference.as_str())
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err(BoardError::Fpga(format!(
            "no component with value {} (the gateware's FPGA); pass --fpga-ref",
            manifest.fpga_ref_part
        ))),
        many => Err(BoardError::Fpga(format!(
            "several {} on the board ({}); pass --fpga-ref",
            manifest.fpga_ref_part,
            many.join(", ")
        ))),
    }
}

/// Direction overlay from a gateware pin map: each pinned ball of the FPGA
/// becomes an output or input.
pub fn gateware_overlay(manifest: &GatewareManifest, fpga_ref: &str) -> DirectionOverlay {
    manifest
        .pins
        .iter()
        .map(|p| {
            let d = match p.dir {
                GatewareDir::Out => PinDir::Out,
                GatewareDir::In => PinDir::In,
            };
            ((fpga_ref.to_string(), p.ball.clone()), d)
        })
        .collect()
}

impl Board {
    /// Bind a schematic, a gateware pin map and part models into a board.
    ///
    /// `models` maps a reference designator to its model. `pulls` declares
    /// net terminations (they also downgrade ERC undriven findings).
    pub fn new(
        sheet: &SchematicSheet,
        manifest: &GatewareManifest,
        fpga_ref: &str,
        models: Vec<(String, Box<dyn PinModel>)>,
        pulls: BTreeMap<String, Pull>,
    ) -> Result<Self, BoardError> {
        let flat = vcad_ecad_schematic::hierarchy::flatten(sheet).sheet;
        let mut opts = NetDriveOptions {
            overlay: gateware_overlay(manifest, fpga_ref),
            pulls: pulls.clone(),
        };
        // Model pins: (ref, pin number) -> (model index, model pin index).
        let mut model_pins: HashMap<String, (usize, usize)> = HashMap::new();
        let mut boxed = Vec::new();
        for (mi, (r, m)) in models.into_iter().enumerate() {
            let comp = flat
                .components
                .iter()
                .find(|c| c.reference == r)
                .ok_or_else(|| BoardError::Model(format!("no component {r} for its model")))?;
            for (pi, (name, dir)) in m.pins().into_iter().enumerate() {
                let pin = comp.pins.iter().find(|p| p.name == name).ok_or_else(|| {
                    BoardError::Model(format!("{r} ({}) has no pin named {name}", comp.value))
                })?;
                opts.overlay.insert((r.clone(), pin.number.clone()), dir);
                model_pins.insert(format!("{r}.{}", pin.number), (mi, pi));
            }
            boxed.push(m);
        }
        let fpga_pins: HashMap<String, usize> = manifest
            .pins
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("{fpga_ref}.{}", p.ball), i))
            .collect();

        let drive = resolve_net_drive(&flat, &opts);
        let erc = net_drive_violations(&drive);
        let end_of = |name: &str| -> End {
            if let Some(&i) = fpga_pins.get(name) {
                End::Fpga(i)
            } else if let Some(&(m, p)) = model_pins.get(name) {
                End::Model(m, p)
            } else {
                End::Unmodelled
            }
        };
        let mut nets = Vec::new();
        let mut unmodelled = Vec::new();
        let mut touched = vec![false; manifest.pins.len()];
        for n in &drive {
            let drivers: Vec<End> = n.drivers.iter().map(|d| end_of(d)).collect();
            let receivers: Vec<End> = n
                .receivers
                .iter()
                .map(|d| end_of(d))
                .filter(|e| *e != End::Unmodelled)
                .collect();
            for e in drivers.iter().chain(&receivers) {
                if let End::Fpga(i) = e {
                    touched[*i] = true;
                }
            }
            if receivers.is_empty() {
                continue;
            }
            for (d, e) in n.drivers.iter().zip(&drivers) {
                if *e == End::Unmodelled {
                    unmodelled.push(d.clone());
                }
            }
            let idle = !matches!(
                n.status,
                NetDriveStatus::Undriven {
                    pull: Some(Pull::Down)
                }
            ) && pulls.get(&n.net) != Some(&Pull::Down);
            nets.push(SimNet {
                drivers,
                receivers,
                idle,
            });
        }
        let floating_inputs = manifest
            .pins
            .iter()
            .enumerate()
            .filter(|(i, p)| p.dir == GatewareDir::In && !touched[*i])
            .map(|(i, _)| i)
            .collect();
        Ok(Self {
            nets,
            models: boxed,
            floating_inputs,
            drive,
            erc,
            fpga_ref: fpga_ref.to_string(),
            unmodelled_drivers: unmodelled,
            cycles: 0,
        })
    }

    /// Cycles simulated so far.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Resolve every net, then advance the FPGA and every model one cycle.
    pub fn step(&mut self, fpga: &mut dyn ChipBackend) {
        for &i in &self.floating_inputs {
            fpga.set_input(i, true);
        }
        for net in &self.nets {
            let level = match net.drivers.as_slice() {
                [End::Fpga(i)] => fpga.output(*i),
                [End::Model(m, p)] => self.models[*m].drive(*p),
                // Undriven, contention, or a lone unmodelled driver: the
                // termination (pull-up unless a pull-down is declared).
                _ => net.idle,
            };
            for r in &net.receivers {
                match *r {
                    End::Fpga(i) => fpga.set_input(i, level),
                    End::Model(m, p) => self.models[m].sense(p, level),
                    End::Unmodelled => {}
                }
            }
        }
        fpga.cycle();
        for m in &mut self.models {
            m.tick();
        }
        self.cycles += 1;
    }
}

/// Console milestones of a Linux boot, in order.
pub const LINUX_MILESTONES: &[(&str, &str)] = &[
    ("bios", "BIOS built on"),
    ("opensbi", "OpenSBI v"),
    ("kernel", "Linux version"),
    ("init", "Run /init"),
    ("login", "buildroot login:"),
];

/// Watches a console byte stream for milestone strings.
#[derive(Debug, Clone)]
pub struct Milestones {
    marks: Vec<(&'static str, &'static str)>,
    tail: Vec<u8>,
    /// Reached milestones: (name, cycle, seconds since start).
    pub reached: Vec<(&'static str, u64, f64)>,
}

impl Milestones {
    /// Watch for `marks` (name, needle).
    pub fn new(marks: &[(&'static str, &'static str)]) -> Self {
        Self {
            marks: marks.to_vec(),
            tail: Vec::new(),
            reached: Vec::new(),
        }
    }

    /// Feed console bytes; returns newly reached milestone names.
    pub fn feed(&mut self, bytes: &[u8], cycle: u64, secs: f64) -> Vec<&'static str> {
        self.tail.extend_from_slice(bytes);
        let mut new = Vec::new();
        for &(name, needle) in &self.marks {
            if self.reached.iter().any(|r| r.0 == name) {
                continue;
            }
            let n = needle.as_bytes();
            if self.tail.windows(n.len()).any(|w| w == n) {
                self.reached.push((name, cycle, secs));
                new.push(name);
            }
        }
        let keep = 256;
        if self.tail.len() > keep {
            self.tail.drain(..self.tail.len() - keep);
        }
        new
    }

    /// Whether `name` was reached.
    pub fn has(&self, name: &str) -> bool {
        self.reached.iter().any(|r| r.0 == name)
    }
}

/// Why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The `until` milestone was reached.
    Reached,
    /// The cycle budget ran out.
    MaxCycles,
}

/// Result of [`run`].
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Cycles simulated.
    pub cycles: u64,
    /// Wall seconds.
    pub secs: f64,
    /// Console bytes received.
    pub console_bytes: usize,
    /// Why it stopped.
    pub stop: StopReason,
}

/// Step `board` + `fpga` until milestone `until` is reached or
/// `max_cycles` have run, feeding the console to `milestones` every few
/// thousand cycles and reporting each new milestone through `on_mark`.
pub fn run(
    board: &mut Board,
    fpga: &mut dyn ChipBackend,
    console: &uart::Console,
    milestones: &mut Milestones,
    max_cycles: u64,
    until: Option<&str>,
    mut on_mark: impl FnMut(&str, u64, f64),
) -> RunOutcome {
    const CHUNK: u64 = 4096;
    let t0 = std::time::Instant::now();
    let mut seen = 0usize;
    let start = board.cycles();
    let mut stop = StopReason::MaxCycles;
    while board.cycles() - start < max_cycles {
        let n = CHUNK.min(max_cycles - (board.cycles() - start));
        for _ in 0..n {
            board.step(fpga);
        }
        if console.len() > seen {
            let new = console.since(seen);
            seen += new.len();
            let secs = t0.elapsed().as_secs_f64();
            for m in milestones.feed(&new, board.cycles(), secs) {
                on_mark(m, board.cycles(), secs);
            }
            if until.is_some_and(|u| milestones.has(u)) {
                stop = StopReason::Reached;
                break;
            }
        }
    }
    RunOutcome {
        cycles: board.cycles() - start,
        secs: t0.elapsed().as_secs_f64(),
        console_bytes: console.len(),
        stop,
    }
}

#[cfg(test)]
mod tests {
    use super::uart::{Console, Ft231x, UartTx};
    use super::*;
    use crate::gateware::GatewareManifest;

    const CLK: u64 = 1_000_000;
    const BAUD: u64 = 50_000;

    fn manifest() -> GatewareManifest {
        serde_json::from_value(serde_json::json!({
            "schema": "vcad.gateware/1", "name": "t", "top": "sim",
            "clock": {"port": "sys_clk", "hz": CLK},
            "rtl": [], "workdir": "rtl",
            "fpga_ref_part": "LFE5U-85F-6BG381C",
            "pins": [
                {"ball": "L4", "signal": "serial.tx", "port": "serial_tx", "dir": "out"},
                {"ball": "M1", "signal": "serial.rx", "port": "serial_rx", "dir": "in"}
            ],
            "cpu": {"file": "x", "sha256": "y"},
            "files": {}
        }))
        .unwrap()
    }

    /// Stand-in gateware: a UART transmitter on L4 sending a greeting, and
    /// a record of what it sees on M1.
    struct FakeFpga {
        tx: UartTx,
        rx_level_seen_low: bool,
    }
    impl ChipBackend for FakeFpga {
        fn set_input(&mut self, pin: usize, level: bool) {
            if pin == 1 && !level {
                self.rx_level_seen_low = true;
            }
        }
        fn output(&self, pin: usize) -> bool {
            pin != 0 || self.tx.level()
        }
        fn cycle(&mut self) {
            self.tx.tick();
        }
    }

    fn run(board_json: &str) -> (Board, Console) {
        let sheet: SchematicSheet = serde_json::from_str(board_json).unwrap();
        let m = manifest();
        let fpga = find_fpga_ref(&sheet, &m, None).unwrap();
        assert_eq!(fpga, "U1");
        let console = Console::default();
        let ft = Ft231x::new(CLK, BAUD, console.clone(), false);
        let mut board = Board::new(
            &sheet,
            &m,
            &fpga,
            vec![("U2".into(), Box::new(ft))],
            BTreeMap::new(),
        )
        .unwrap();
        let mut f = FakeFpga {
            tx: UartTx::new(CLK, BAUD),
            rx_level_seen_low: false,
        };
        f.tx.send(b"BIOS built on hello\n");
        for _ in 0..(CLK / BAUD) * 10 * 25 {
            board.step(&mut f);
        }
        assert!(!f.rx_level_seen_low, "FTDI TXD idles high");
        (board, console)
    }

    #[test]
    fn correct_board_carries_console_bytes() {
        let (board, console) = run(include_str!(
            "../../../vcad-ecad-schematic/src/fixtures/ulx3s_uart.json"
        ));
        assert!(board.erc.is_empty(), "{:?}", board.erc);
        assert_eq!(console.bytes(), b"BIOS built on hello\n");
    }

    #[test]
    fn swapped_board_is_silent_and_erc_says_why() {
        let (board, console) = run(include_str!(
            "../../../vcad-ecad-schematic/src/fixtures/ulx3s_uart_miswire.json"
        ));
        assert!(console.bytes().is_empty(), "{:?}", console.bytes());
        let msgs: Vec<&str> = board.erc.iter().map(|v| v.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.starts_with("CONTENTION on FTDI_TXD")));
        assert!(msgs.iter().any(|m| m.starts_with("UNDRIVEN FTDI_RXD")));
    }

    #[test]
    fn milestones_span_chunk_boundaries() {
        let mut m = Milestones::new(LINUX_MILESTONES);
        assert!(m.feed(b"...BIOS bu", 10, 0.1).is_empty());
        assert_eq!(m.feed(b"ilt on Nov", 20, 0.2), vec!["bios"]);
        assert!(m.feed(b"BIOS built on", 30, 0.3).is_empty());
        assert!(m.has("bios") && !m.has("kernel"));
    }
}
