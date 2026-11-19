//! Pinned gateware artifacts: sha256-addressed tarballs of generated HDL.
//!
//! vcad never runs the HDL generator (LiteX is Python). The generator runs
//! once, offline, and its output is frozen into a tarball whose sha256 is the
//! artifact's identity. vcad consumes the Verilog.
//!
//! Tarball layout (one top-level `gateware/` directory):
//!
//! ```text
//! gateware/manifest.json   GatewareManifest (schema "vcad.gateware/1")
//! gateware/rtl/*.v         Verilog sources + $readmemh init files
//! gateware/csr.json        SoC address map (LiteX)
//! gateware/images/*        boot images baked into the RAM init
//! gateware/gen/*           the generator script + container it ran in
//! ```
//!
//! The manifest lists the sha256 of every other file; [`Artifact::open`]
//! re-hashes them all and fails closed on any mismatch, missing or extra
//! file. The tarball's own sha256 is checked before extraction.
//!
//! [`ArtifactStore`] caches extracted artifacts under
//! `$VCAD_GATEWARE_DIR` (default `~/.vcad/gateware/<sha>/`) and fetches
//! missing ones from `$VCAD_GATEWARE_URL` (a URL template with `{sha}`), or
//! the default GitHub release location. Fetch and extract shell out to
//! `curl` and `tar`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The M1 Linux-board gateware: linux-on-litex VexRiscv-SMP, console UART
/// on the ULX3S pads (L4 = `serial.tx`, M1 = `serial.rx`), boot images
/// preloaded into the SDRAM model. See `docs/linux-board-sim.md`.
pub const ULX3S_UART_LINUX: &str =
    "63d8d63f05f42bc15655e9ee1bf070b3d2b0f47c6c9e4b30642accc0d6c7df93";

/// Manifest schema identifier this module reads.
pub const SCHEMA: &str = "vcad.gateware/1";

/// Default fetch location; `{sha}` is the full tarball sha256, `{sha12}`
/// its first 12 hex digits.
pub const DEFAULT_URL: &str =
    "https://github.com/muni-works/vcad/releases/download/gateware-{sha12}/gateware-{sha}.tar.gz";

/// Errors from loading, verifying or fetching an artifact.
#[derive(Debug, thiserror::Error)]
pub enum GatewareError {
    /// Filesystem or subprocess I/O failed.
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    /// The manifest did not parse or has the wrong schema.
    #[error("manifest: {0}")]
    Manifest(String),
    /// A hash did not match.
    #[error("sha256 mismatch for {what}: expected {expected}, got {actual}")]
    Hash {
        /// What was hashed.
        what: String,
        /// Pinned hash.
        expected: String,
        /// Computed hash.
        actual: String,
    },
    /// Fetch or extract failed.
    #[error("{0}")]
    Fetch(String),
}

/// Direction of a gateware top-level pin, from the FPGA's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewareDir {
    /// The gateware drives the ball.
    Out,
    /// The gateware reads the ball.
    In,
}

/// One gateware signal and the package ball it is constrained to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewarePin {
    /// Package ball (the schematic pin number on the FPGA symbol), e.g. `L4`.
    pub ball: String,
    /// Gateware signal, e.g. `serial.tx`.
    pub signal: String,
    /// Verilog top-level port carrying it, e.g. `serial_tx`.
    pub port: String,
    /// Direction.
    pub dir: GatewareDir,
}

/// The clock the model is stepped on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClockSpec {
    /// Verilog top-level clock port.
    pub port: String,
    /// The frequency the SoC was generated for (timer constants, baud
    /// divisors). The simulator steps cycles, not seconds.
    pub hz: u64,
}

/// Console UART framing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UartSpec {
    /// Baud rate.
    pub baud: u64,
    /// Frame format, e.g. `8N1`.
    pub frame: String,
}

/// CPU netlist identity (the part that must match the bitstream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuSpec {
    /// Path of the CPU Verilog inside the artifact.
    pub file: String,
    /// Its sha256.
    pub sha256: String,
    /// Provenance note.
    #[serde(default)]
    pub note: String,
}

/// `gateware/manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewareManifest {
    /// Must be [`SCHEMA`].
    pub schema: String,
    /// Short name.
    pub name: String,
    /// What the gateware is.
    #[serde(default)]
    pub description: String,
    /// Verilog top module.
    pub top: String,
    /// Clock port and frequency.
    pub clock: ClockSpec,
    /// Top-level inputs held at 0 (e.g. LiteX's `sim_trace`).
    #[serde(default)]
    pub tie_low: Vec<String>,
    /// Verilog sources, relative to the artifact root.
    pub rtl: Vec<String>,
    /// Directory (relative) the model must run in: `$readmemh` paths in the
    /// Verilog are relative to it.
    pub workdir: String,
    /// Console UART, if the gateware has one.
    #[serde(default)]
    pub uart: Option<UartSpec>,
    /// The FPGA part the pin map targets (matched against the schematic
    /// component value).
    pub fpga_ref_part: String,
    /// Pin map: ball → signal → port → direction.
    pub pins: Vec<GatewarePin>,
    /// CPU identity.
    pub cpu: CpuSpec,
    /// Boot image provenance (free-form).
    #[serde(default)]
    pub images: serde_json::Value,
    /// Generator provenance: script, container, upstream commits.
    #[serde(default)]
    pub generator: serde_json::Value,
    /// sha256 of every file in the artifact except `manifest.json`.
    pub files: BTreeMap<String, String>,
}

impl GatewareManifest {
    /// Parse and schema-check a manifest.
    pub fn from_json(s: &str) -> Result<Self, GatewareError> {
        let m: Self =
            serde_json::from_str(s).map_err(|e| GatewareError::Manifest(e.to_string()))?;
        if m.schema != SCHEMA {
            return Err(GatewareError::Manifest(format!(
                "schema {:?}, expected {SCHEMA:?}",
                m.schema
            )));
        }
        for f in m.rtl.iter().chain(std::iter::once(&m.cpu.file)) {
            if !m.files.contains_key(f) {
                return Err(GatewareError::Manifest(format!("{f} is not in files")));
            }
        }
        if m.files.get(&m.cpu.file) != Some(&m.cpu.sha256) {
            return Err(GatewareError::Manifest(
                "cpu.sha256 disagrees with files".into(),
            ));
        }
        Ok(m)
    }

    /// Every top-level input port the model exposes, in a stable order:
    /// clock, tied-low ports, then the pin-map ports.
    pub fn ports(&self) -> Vec<String> {
        let mut v = vec![self.clock.port.clone()];
        v.extend(self.tie_low.iter().cloned());
        v.extend(self.pins.iter().map(|p| p.port.clone()));
        v
    }
}

/// Lowercase hex sha256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String, GatewareError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn io(path: &Path, e: std::io::Error) -> GatewareError {
    GatewareError::Io(path.display().to_string(), e)
}

/// An extracted, verified artifact.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// The `gateware/` directory.
    pub root: PathBuf,
    /// Its manifest.
    pub manifest: GatewareManifest,
}

impl Artifact {
    /// Open an extracted `gateware/` directory, re-hashing every file
    /// against the manifest. Fails on a mismatch, a missing file, or a file
    /// the manifest does not list.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, GatewareError> {
        let root = root.into();
        let mpath = root.join("manifest.json");
        let text = std::fs::read_to_string(&mpath).map_err(|e| io(&mpath, e))?;
        let manifest = GatewareManifest::from_json(&text)?;
        let mut on_disk = Vec::new();
        walk(&root, &root, &mut on_disk)?;
        for rel in &on_disk {
            if rel != "manifest.json" && !manifest.files.contains_key(rel) {
                return Err(GatewareError::Manifest(format!(
                    "{rel} is in the artifact but not in the manifest"
                )));
            }
        }
        for (rel, expected) in &manifest.files {
            let actual = sha256_file(&root.join(rel))?;
            if &actual != expected {
                return Err(GatewareError::Hash {
                    what: rel.clone(),
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        Ok(Self { root, manifest })
    }

    /// Absolute paths of the Verilog sources.
    pub fn rtl_paths(&self) -> Vec<PathBuf> {
        self.manifest
            .rtl
            .iter()
            .map(|f| self.root.join(f))
            .collect()
    }

    /// Absolute path of the model's working directory.
    pub fn workdir(&self) -> PathBuf {
        self.root.join(&self.manifest.workdir)
    }
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), GatewareError> {
    for e in std::fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let e = e.map_err(|e| io(dir, e))?;
        let p = e.path();
        if p.is_dir() {
            walk(base, &p, out)?;
        } else {
            let rel = p.strip_prefix(base).unwrap_or(&p);
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Local cache of extracted artifacts, keyed by tarball sha256.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    /// Cache root.
    pub root: PathBuf,
    /// Fetch URL template (`{sha}`, `{sha12}`).
    pub url_template: String,
}

impl ArtifactStore {
    /// Store at `$VCAD_GATEWARE_DIR` or `~/.vcad/gateware`, fetching from
    /// `$VCAD_GATEWARE_URL` or [`DEFAULT_URL`].
    pub fn from_env() -> Self {
        let root = std::env::var_os("VCAD_GATEWARE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").unwrap_or_default();
                PathBuf::from(home).join(".vcad").join("gateware")
            });
        let url_template =
            std::env::var("VCAD_GATEWARE_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
        Self { root, url_template }
    }

    /// Directory an artifact extracts to.
    pub fn dir_for(&self, sha: &str) -> PathBuf {
        self.root.join(sha)
    }

    /// Resolve a full sha256 to a verified artifact: the cached extraction,
    /// else a cached tarball, else a fetch.
    pub fn resolve(&self, sha: &str) -> Result<Artifact, GatewareError> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GatewareError::Manifest(format!(
                "artifact id must be a full 64-hex sha256, got {sha:?}"
            )));
        }
        let sha = sha.to_ascii_lowercase();
        let extracted = self.dir_for(&sha).join("gateware");
        if extracted.join("manifest.json").exists() {
            return Artifact::open(extracted);
        }
        let tgz = self.root.join(format!("gateware-{sha}.tar.gz"));
        if !tgz.exists() {
            self.fetch(&sha, &tgz)?;
        }
        self.import_tarball(&tgz, Some(&sha))
    }

    fn fetch(&self, sha: &str, dest: &Path) -> Result<(), GatewareError> {
        std::fs::create_dir_all(&self.root).map_err(|e| io(&self.root, e))?;
        let url = self
            .url_template
            .replace("{sha12}", &sha[..12])
            .replace("{sha}", sha);
        let part = dest.with_extension("part");
        let status = Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map_err(|e| GatewareError::Fetch(format!("curl: {e}")))?;
        if !status.success() {
            let _ = std::fs::remove_file(&part);
            return Err(GatewareError::Fetch(format!(
                "fetch {url} failed ({status})"
            )));
        }
        std::fs::rename(&part, dest).map_err(|e| io(dest, e))
    }

    /// Verify a tarball (against `expect` if given), extract it into the
    /// store and open it. Returns the verified artifact.
    pub fn import_tarball(
        &self,
        tgz: &Path,
        expect: Option<&str>,
    ) -> Result<Artifact, GatewareError> {
        let sha = sha256_file(tgz)?;
        if let Some(e) = expect {
            if !e.eq_ignore_ascii_case(&sha) {
                return Err(GatewareError::Hash {
                    what: tgz.display().to_string(),
                    expected: e.to_string(),
                    actual: sha,
                });
            }
        }
        let final_dir = self.dir_for(&sha);
        if final_dir.join("gateware/manifest.json").exists() {
            return Artifact::open(final_dir.join("gateware"));
        }
        let tmp = self.root.join(format!("{sha}.tmp{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
        let status = Command::new("tar")
            .arg("xzf")
            .arg(tgz)
            .arg("-C")
            .arg(&tmp)
            .status()
            .map_err(|e| GatewareError::Fetch(format!("tar: {e}")))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(GatewareError::Fetch(format!(
                "extract {} failed",
                tgz.display()
            )));
        }
        let art = match Artifact::open(tmp.join("gateware")) {
            Ok(a) => a,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        let _ = std::fs::remove_dir_all(&final_dir);
        std::fs::rename(&tmp, &final_dir).map_err(|e| io(&final_dir, e))?;
        Ok(Artifact {
            root: final_dir.join("gateware"),
            manifest: art.manifest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vcad-gw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write a minimal artifact tree and return its `gateware/` dir.
    fn tiny_artifact(base: &Path) -> PathBuf {
        let g = base.join("gateware");
        std::fs::create_dir_all(g.join("rtl")).unwrap();
        let v = b"module sim(input wire sys_clk, output reg serial_tx, input wire serial_rx); endmodule\n";
        std::fs::write(g.join("rtl/sim.v"), v).unwrap();
        let mut files = BTreeMap::new();
        files.insert("rtl/sim.v".to_string(), sha256_hex(v));
        let m = GatewareManifest {
            schema: SCHEMA.into(),
            name: "tiny".into(),
            description: String::new(),
            top: "sim".into(),
            clock: ClockSpec {
                port: "sys_clk".into(),
                hz: 100_000_000,
            },
            tie_low: vec![],
            rtl: vec!["rtl/sim.v".into()],
            workdir: "rtl".into(),
            uart: Some(UartSpec {
                baud: 115_200,
                frame: "8N1".into(),
            }),
            fpga_ref_part: "LFE5U-85F-6BG381C".into(),
            pins: vec![GatewarePin {
                ball: "L4".into(),
                signal: "serial.tx".into(),
                port: "serial_tx".into(),
                dir: GatewareDir::Out,
            }],
            cpu: CpuSpec {
                file: "rtl/sim.v".into(),
                sha256: sha256_hex(v),
                note: String::new(),
            },
            images: serde_json::Value::Null,
            generator: serde_json::Value::Null,
            files,
        };
        std::fs::write(
            g.join("manifest.json"),
            serde_json::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        g
    }

    #[test]
    fn open_verifies_and_rejects_tampering() {
        let base = scratch("open");
        let g = tiny_artifact(&base);
        let a = Artifact::open(&g).unwrap();
        assert_eq!(a.manifest.ports(), vec!["sys_clk", "serial_tx"]);

        std::fs::write(g.join("rtl/sim.v"), b"tampered").unwrap();
        assert!(matches!(
            Artifact::open(&g),
            Err(GatewareError::Hash { .. })
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_rejects_unlisted_file() {
        let base = scratch("extra");
        let g = tiny_artifact(&base);
        std::fs::write(g.join("rtl/sneaky.v"), b"x").unwrap();
        assert!(matches!(
            Artifact::open(&g),
            Err(GatewareError::Manifest(_))
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_imports_by_sha_and_rejects_wrong_sha() {
        let base = scratch("store");
        tiny_artifact(&base);
        let tgz = base.join("a.tar.gz");
        let ok = Command::new("tar")
            .arg("czf")
            .arg(&tgz)
            .arg("-C")
            .arg(&base)
            .arg("gateware")
            .status()
            .unwrap();
        assert!(ok.success());
        let store = ArtifactStore {
            root: base.join("cache"),
            url_template: "file:///nonexistent/{sha}".into(),
        };
        let wrong = "0".repeat(64);
        assert!(matches!(
            store.import_tarball(&tgz, Some(&wrong)),
            Err(GatewareError::Hash { .. })
        ));
        let a = store.import_tarball(&tgz, None).unwrap();
        let sha = sha256_file(&tgz).unwrap();
        assert!(a.root.starts_with(store.dir_for(&sha)));
        // Second resolve hits the extracted cache, no fetch.
        let b = store.resolve(&sha).unwrap();
        assert_eq!(b.manifest.name, "tiny");
        assert!(store.resolve("abc").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
