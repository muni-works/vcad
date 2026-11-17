//! REW impedance export → measured port tuning frequency `f_b`.
//!
//! Room EQ Wizard's "Export impedance data as text" writes comment lines
//! starting with `*`, then rows of `frequency_hz |Z|_ohm phase_deg`. A vented
//! box shows two impedance peaks; its tuning frequency is the |Z| minimum
//! (the saddle) between the two largest. That number binds to the predicted
//! `acoustics.tuning_hz` claim (see [`crate::receipt`]).
//!
//! Fail-closed: non-numeric rows, too few rows, fewer than two peaks (a
//! sealed box or a truncated sweep), or peaks on adjacent samples are
//! errors, never guesses.

use serde::Serialize;

/// The saddle reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RewTuning {
    /// Measured tuning frequency, Hz (the |Z| minimum between the peaks).
    pub f_b_hz: f64,
    /// Half the local frequency step around the minimum — the sweep's own
    /// resolution floor, Hz.
    pub grid_half_step_hz: f64,
    /// One-sigma uncertainty, Hz: the grid floor combined in quadrature with
    /// any extra uncertainty the measurer declared.
    pub uncertainty_hz: f64,
    /// The two impedance peaks bracketing the saddle, Hz.
    pub peaks_hz: [f64; 2],
}

/// Numeric rows of a text export: `*`/`#`/`!`/`;` comment lines and one
/// leading header line are skipped; separators are whitespace, `,`, `;`, tab.
pub(crate) fn numeric_rows(
    text: &str,
    min_cols: usize,
    what: &str,
) -> Result<Vec<Vec<f64>>, String> {
    let mut rows = Vec::new();
    let mut header_seen = false;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(['*', '#', '!', ';']) {
            continue;
        }
        let toks: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .filter(|t| !t.is_empty())
            .collect();
        let nums: Result<Vec<f64>, _> = toks.iter().map(|t| t.parse::<f64>()).collect();
        match nums {
            Ok(n) if n.iter().all(|v| v.is_finite()) => {
                if n.len() < min_cols {
                    return Err(format!(
                        "{what} line {}: expected at least {min_cols} columns",
                        i + 1
                    ));
                }
                rows.push(n);
            }
            _ => {
                if !header_seen && rows.is_empty() && line.chars().any(|c| c.is_ascii_alphabetic())
                {
                    header_seen = true;
                    continue;
                }
                return Err(format!("{what} line {}: non-numeric data", i + 1));
            }
        }
    }
    Ok(rows)
}

/// Read the port tuning frequency from a REW impedance export.
/// `extra_uncertainty_hz` (≥ 0) is combined with the sweep's grid floor.
pub fn tuning_from_impedance_export(
    text: &str,
    extra_uncertainty_hz: f64,
) -> Result<RewTuning, String> {
    if !(extra_uncertainty_hz.is_finite() && extra_uncertainty_hz >= 0.0) {
        return Err("extra uncertainty must be finite and >= 0".into());
    }
    let mut rows: Vec<(f64, f64)> = numeric_rows(text, 2, "REW impedance export")?
        .into_iter()
        .map(|r| (r[0], r[1]))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    if rows.len() < 5 {
        return Err("REW impedance export: need at least 5 rows".into());
    }
    let peaks: Vec<usize> = (1..rows.len() - 1)
        .filter(|&i| rows[i].1 > rows[i - 1].1 && rows[i].1 >= rows[i + 1].1)
        .collect();
    if peaks.len() < 2 {
        return Err(format!(
            "REW impedance export: found {} impedance peak(s); a vented box shows two — is this a sealed box or a truncated sweep?",
            peaks.len()
        ));
    }
    let mut by_height = peaks.clone();
    by_height.sort_by(|&a, &b| rows[b].1.total_cmp(&rows[a].1));
    let (pa, pb) = {
        let (x, y) = (by_height[0], by_height[1]);
        (x.min(y), x.max(y))
    };
    if pb - pa < 2 {
        return Err("REW impedance export: the two peaks are adjacent samples — sweep resolution too coarse".into());
    }
    let min = (pa + 1..pb)
        .min_by(|&a, &b| rows[a].1.total_cmp(&rows[b].1))
        .expect("a sample between the peaks");
    let half = (rows[min].0 - rows[min - 1].0).max(rows[min + 1].0 - rows[min].0) / 2.0;
    Ok(RewTuning {
        f_b_hz: rows[min].0,
        grid_half_step_hz: half,
        uncertainty_hz: extra_uncertainty_hz.hypot(half),
        peaks_hz: [rows[pa].0, rows[pb].0],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vented_box() -> String {
        let mut s = String::from(
            "* REW impedance export (synthetic)\n* Freq(Hz) Magnitude(ohm) Phase(deg)\n",
        );
        for f in 10..=200 {
            let f = f as f64;
            let peak = |fc: f64, h: f64, w: f64| h / (1.0 + ((f - fc) / w).powi(2));
            let dip = if f == 45.0 { 0.5 } else { 0.0 };
            s.push_str(&format!(
                "{f:.3} {:.4} 0.0\n",
                6.0 + peak(25.0, 30.0, 5.0) + peak(80.0, 22.0, 10.0) - dip
            ));
        }
        s
    }

    #[test]
    fn finds_the_saddle_between_the_two_peaks() {
        let t = tuning_from_impedance_export(&vented_box(), 0.0).unwrap();
        assert_eq!(t.f_b_hz, 45.0);
        assert_eq!(t.peaks_hz, [25.0, 80.0]);
        assert_eq!(t.grid_half_step_hz, 0.5);
        let t = tuning_from_impedance_export(&vented_box(), 1.2).unwrap();
        assert!((t.uncertainty_hz - 1.3).abs() < 1e-12);
    }

    #[test]
    fn refuses_instead_of_guessing() {
        // Monotonic: no peaks (sealed box / truncated sweep).
        assert!(
            tuning_from_impedance_export("10 5 0\n11 6 0\n12 7 0\n13 8 0\n14 9 0", 0.0)
                .unwrap_err()
                .contains("peak")
        );
        assert!(tuning_from_impedance_export("10 5\n11 x\n", 0.0).is_err());
        assert!(tuning_from_impedance_export("10 5\n", 0.0).is_err());
        assert!(tuning_from_impedance_export(&vented_box(), -1.0).is_err());
    }
}
