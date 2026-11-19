//! Net drive resolution: who drives each net, the way copper sees it.
//!
//! Per net, count the pins that actively drive it:
//! - exactly one driver → [`NetDriveStatus::Driven`]
//! - none, while something listens → [`NetDriveStatus::Undriven`] (an error,
//!   or a warning when a pull-up/pull-down is declared for the net)
//! - two or more push-pull drivers → [`NetDriveStatus::Contention`]
//!
//! Directions come from the schematic's [`PinType`]s, optionally overridden
//! per pin by a [`DirectionOverlay`]. The overlay is how a programmable part
//! gets its real direction: an FPGA ball is `Bidirectional` in the symbol,
//! but the gateware's pin map fixes it as an output (`serial.tx`) or an input
//! (`serial.rx`). Without an override a `Bidirectional` pin is neither driver
//! nor receiver, so an unconfigured ball never invents a fault.
//!
//! This is a separate pass from [`crate::erc::check_erc`]: it needs the
//! overlay to say anything useful about programmable parts, and the board
//! simulator resolves its nets with the same rules every cycle.

use std::collections::{BTreeMap, HashMap};
use vcad_ir::ecad::*;

use crate::erc::{ErcSeverity, ErcViolation};
use crate::generate_netlist;

/// A resolved pin direction, as one end of a net sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinDir {
    /// Push-pull driver (a second one on the net is contention).
    Out,
    /// Shared driver: tri-state, open-collector/emitter. Drives the net but
    /// may share it with others without contention.
    Shared,
    /// Receiver.
    In,
    /// Neither drives nor listens (passive, no-connect, unconfigured bidir).
    None,
}

impl PinDir {
    /// Direction implied by a schematic pin type alone.
    pub fn from_pin_type(t: PinType) -> Self {
        match t {
            PinType::Output | PinType::PowerOutput => PinDir::Out,
            PinType::TriState | PinType::OpenCollector | PinType::OpenEmitter => PinDir::Shared,
            PinType::Input | PinType::PowerInput => PinDir::In,
            PinType::Bidirectional | PinType::Passive | PinType::NotConnected | PinType::Free => {
                PinDir::None
            }
        }
    }
}

/// Per-pin direction overrides keyed by `(component ref, pin number)`.
pub type DirectionOverlay = HashMap<(String, String), PinDir>;

/// A declared termination on a net.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pull {
    /// Pulled to the high rail.
    Up,
    /// Pulled to ground.
    Down,
}

/// Inputs to [`resolve_net_drive`] beyond the sheet.
#[derive(Debug, Clone, Default)]
pub struct NetDriveOptions {
    /// Pin direction overrides (e.g. from a gateware pin map).
    pub overlay: DirectionOverlay,
    /// Declared pulls by net name.
    pub pulls: BTreeMap<String, Pull>,
}

/// Drive state of one net.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NetDriveStatus {
    /// Exactly one push-pull driver, or only shared drivers.
    Driven,
    /// Something listens but nothing drives. `pull` is the declared
    /// termination, which sets the level receivers see.
    Undriven {
        /// Declared pull, if any.
        pull: Option<Pull>,
    },
    /// Two or more push-pull drivers fight.
    Contention,
    /// Nothing drives and nothing listens.
    Unused,
}

/// One net's resolved drivers and receivers.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NetDrive {
    /// Net name.
    pub net: String,
    /// Driving pins (`"REF.pin"`), push-pull and shared.
    pub drivers: Vec<String>,
    /// Receiving pins (`"REF.pin"`).
    pub receivers: Vec<String>,
    /// Resolution.
    pub status: NetDriveStatus,
}

/// Resolve every net's drivers. Nets come back sorted by name.
///
/// Power flags count as a driver on their net. Several power-output pins of
/// one component are a single source (paralleled regulator outputs).
pub fn resolve_net_drive(sheet: &SchematicSheet, opts: &NetDriveOptions) -> Vec<NetDrive> {
    let flat = crate::hierarchy::flatten(sheet);
    let sheet = &flat.sheet;
    let netlist = generate_netlist(sheet);
    let flagged = crate::erc::flagged_net_names(sheet, &netlist);

    let mut pin_info: HashMap<(&str, &str), PinType> = HashMap::new();
    for comp in &sheet.components {
        for pin in &comp.pins {
            pin_info.insert((comp.reference.as_str(), pin.number.as_str()), pin.pin_type);
        }
    }

    let mut out: Vec<NetDrive> = netlist
        .nets
        .iter()
        .map(|net| {
            let mut drivers = Vec::new();
            let mut receivers = Vec::new();
            // Push-pull sources, deduplicated per component for power pins.
            let mut strong: Vec<String> = Vec::new();
            for c in &net.connections {
                let key = (c.component_ref.clone(), c.pin_number.clone());
                let pin_type = pin_info
                    .get(&(c.component_ref.as_str(), c.pin_number.as_str()))
                    .copied();
                let dir = opts
                    .overlay
                    .get(&key)
                    .copied()
                    .or(pin_type.map(PinDir::from_pin_type))
                    .unwrap_or(PinDir::None);
                let name = format!("{}.{}", c.component_ref, c.pin_number);
                match dir {
                    PinDir::Out => {
                        let source = if pin_type == Some(PinType::PowerOutput) {
                            c.component_ref.clone()
                        } else {
                            name.clone()
                        };
                        if !strong.contains(&source) {
                            strong.push(source);
                        }
                        drivers.push(name);
                    }
                    PinDir::Shared => drivers.push(name),
                    PinDir::In => receivers.push(name),
                    PinDir::None => {}
                }
            }
            let flag = flagged.contains(net.name.as_str());
            let status = if strong.len() > 1 {
                NetDriveStatus::Contention
            } else if !drivers.is_empty() || flag {
                NetDriveStatus::Driven
            } else if !receivers.is_empty() {
                NetDriveStatus::Undriven {
                    pull: opts.pulls.get(&net.name).copied(),
                }
            } else {
                NetDriveStatus::Unused
            };
            NetDrive {
                net: net.name.clone(),
                drivers,
                receivers,
                status,
            }
        })
        .collect();
    out.sort_by(|a, b| a.net.cmp(&b.net));
    out
}

/// ERC findings for resolved nets: contention and unterminated undriven
/// nets are errors, an undriven net held by a declared pull is a warning.
pub fn net_drive_violations(nets: &[NetDrive]) -> Vec<ErcViolation> {
    let mut v = Vec::new();
    for n in nets {
        let listeners = if n.receivers.is_empty() {
            "nothing".to_string()
        } else {
            n.receivers.join(", ")
        };
        match n.status {
            NetDriveStatus::Contention => v.push(ErcViolation {
                severity: ErcSeverity::Error,
                message: format!(
                    "CONTENTION on {}: {} all drive; {} listens",
                    n.net,
                    n.drivers.join(", "),
                    listeners
                ),
                position: None,
            }),
            NetDriveStatus::Undriven { pull } => v.push(ErcViolation {
                severity: if pull.is_some() {
                    ErcSeverity::Warning
                } else {
                    ErcSeverity::Error
                },
                message: match pull {
                    Some(p) => format!(
                        "UNDRIVEN {}: {} held by pull-{}",
                        n.net,
                        listeners,
                        if p == Pull::Up { "up" } else { "down" }
                    ),
                    None => format!("UNDRIVEN {}: {} float", n.net, listeners),
                },
                position: None,
            }),
            NetDriveStatus::Driven | NetDriveStatus::Unused => {}
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(json: &str) -> SchematicSheet {
        serde_json::from_str(json).expect("fixture parses as SchematicSheet")
    }

    /// The ULX3S gateware pin map for the console UART:
    /// L4 = serial.tx (out), M1 = serial.rx (in).
    fn ulx3s_uart_overlay() -> NetDriveOptions {
        let mut opts = NetDriveOptions::default();
        opts.overlay.insert(("U1".into(), "L4".into()), PinDir::Out);
        opts.overlay.insert(("U1".into(), "M1".into()), PinDir::In);
        opts
    }

    fn status_of<'a>(nets: &'a [NetDrive], name: &str) -> &'a NetDriveStatus {
        &nets.iter().find(|n| n.net == name).expect(name).status
    }

    #[test]
    fn ulx3s_uart_subset_is_clean() {
        let s = sheet(include_str!("fixtures/ulx3s_uart.json"));
        let nets = resolve_net_drive(&s, &ulx3s_uart_overlay());
        assert_eq!(status_of(&nets, "FTDI_RXD"), &NetDriveStatus::Driven);
        assert_eq!(status_of(&nets, "FTDI_TXD"), &NetDriveStatus::Driven);
        // RTS/DTR land on unconfigured balls: driven by the FTDI, no listener.
        assert_eq!(status_of(&nets, "FTDI_RTS"), &NetDriveStatus::Driven);
        assert!(net_drive_violations(&nets).is_empty());
    }

    #[test]
    fn txrx_swap_reports_contention_and_undriven() {
        let s = sheet(include_str!("fixtures/ulx3s_uart_miswire.json"));
        let nets = resolve_net_drive(&s, &ulx3s_uart_overlay());
        assert_eq!(status_of(&nets, "FTDI_TXD"), &NetDriveStatus::Contention);
        assert_eq!(
            status_of(&nets, "FTDI_RXD"),
            &NetDriveStatus::Undriven { pull: None }
        );
        let v = net_drive_violations(&nets);
        let msgs: Vec<&str> = v.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(v.len(), 2, "{msgs:?}");
        assert!(v.iter().all(|x| x.severity == ErcSeverity::Error));
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("CONTENTION on FTDI_TXD: U1.L4, U2.1")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("UNDRIVEN FTDI_RXD: U1.M1, U2.5 float")),
            "{msgs:?}"
        );
    }

    #[test]
    fn declared_pull_downgrades_undriven_to_warning() {
        let s = sheet(include_str!("fixtures/ulx3s_uart_miswire.json"));
        let mut opts = ulx3s_uart_overlay();
        opts.pulls.insert("FTDI_RXD".into(), Pull::Up);
        let nets = resolve_net_drive(&s, &opts);
        assert_eq!(
            status_of(&nets, "FTDI_RXD"),
            &NetDriveStatus::Undriven {
                pull: Some(Pull::Up)
            }
        );
        let v = net_drive_violations(&nets);
        let w = v.iter().find(|x| x.message.contains("FTDI_RXD")).unwrap();
        assert_eq!(w.severity, ErcSeverity::Warning);
    }

    #[test]
    fn without_overlay_bidir_balls_are_silent() {
        // No gateware pin map: the FPGA balls stay Bidirectional, so the
        // swap is invisible — the resolver never invents a fault.
        let s = sheet(include_str!("fixtures/ulx3s_uart_miswire.json"));
        let nets = resolve_net_drive(&s, &NetDriveOptions::default());
        // FTDI_RXD: only U2.5 (Input) listens, nobody drives.
        assert_eq!(
            status_of(&nets, "FTDI_RXD"),
            &NetDriveStatus::Undriven { pull: None }
        );
        assert_eq!(status_of(&nets, "FTDI_TXD"), &NetDriveStatus::Driven);
    }

    #[test]
    fn paralleled_power_outputs_of_one_part_are_one_source() {
        let s = sheet(
            r#"{"components":[
              {"ref":"U1","value":"LDO","footprintId":"x","position":{"x":0,"y":0},"pins":[
                {"number":"1","name":"OUT","pin_type":"PowerOutput","position":{"x":0,"y":0}},
                {"number":"2","name":"OUT","pin_type":"PowerOutput","position":{"x":0,"y":10}}]},
              {"ref":"U2","value":"MCU","footprintId":"x","position":{"x":100,"y":0},"pins":[
                {"number":"1","name":"VDD","pin_type":"PowerInput","position":{"x":0,"y":0}}]}],
             "nets":{"3V3":["U1.1","U1.2","U2.1"]}}"#,
        );
        let nets = resolve_net_drive(&s, &NetDriveOptions::default());
        assert_eq!(status_of(&nets, "3V3"), &NetDriveStatus::Driven);
    }
}
