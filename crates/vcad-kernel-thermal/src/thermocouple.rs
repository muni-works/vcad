//! Thermocouple log (CSV) → one measured temperature for a thermal claim.
//!
//! The log needs a header row naming a time column (`time_s`, `t`, `time`,
//! `seconds`, `elapsed_s`, …; unit suffixes in parentheses are ignored) and a
//! temperature column (named, or the single column that looks like one).
//! Two reductions:
//!
//! - **Steady** (default): the mean over the last window (default the last
//!   10% of the log, at least 5 samples). Refused while the log is still
//!   drifting by more than `max_drift_c` across the window — a heating part
//!   has no steady-state temperature yet, and reporting one would bind a
//!   wrong number to a `t_max_c` claim.
//! - **Max**: the peak sample, for transient peak claims (`t_max_peak_c`).
//!
//! Uncertainty is the probe/meter uncertainty combined in quadrature with the
//! window's sample spread. Fail-closed on anything malformed.

use serde::{Deserialize, Serialize};

/// Reduction options.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThermocoupleOptions {
    /// Temperature column name; inferred when the header has exactly one.
    #[serde(default)]
    pub column: Option<String>,
    /// `"steady"` (default) or `"max"`.
    #[serde(default)]
    pub reduce: Option<String>,
    /// Steady-state window, s (default: last 10% of the log).
    #[serde(default)]
    pub window_s: Option<f64>,
    /// Allowed drift across the window, °C (default 0.5).
    #[serde(default)]
    pub max_drift_c: Option<f64>,
    /// Probe + meter one-sigma uncertainty, °C (required, ≥ 0). Type-K
    /// standard limits are ±2.2 °C.
    pub instrument_uncertainty_c: f64,
}

/// The reduced reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThermocoupleReading {
    /// Measured temperature, °C.
    pub value_c: f64,
    /// One-sigma uncertainty, °C (instrument ⊕ window spread).
    pub uncertainty_c: f64,
    /// Sample standard deviation over the window, °C (0 for `max`).
    pub spread_c: f64,
    /// Samples reduced.
    pub samples: usize,
    /// Window span, s (steady only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_s: Option<f64>,
    /// Linear drift across the window, °C (steady only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_c: Option<f64>,
    /// The temperature column used.
    pub column: String,
}

fn clean(h: &str) -> String {
    let mut s = String::new();
    let mut depth = 0;
    for c in h.trim().chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth > 0 => {}
            c if c.is_ascii_alphanumeric() || c == '°' => s.extend(c.to_lowercase()),
            _ => s.push('_'),
        }
    }
    s.trim_matches('_').to_string()
}

fn split(line: &str) -> Vec<&str> {
    line.split([',', ';', '\t']).map(str::trim).collect()
}

/// Reduce a thermocouple CSV log to one temperature.
pub fn reduce_log(text: &str, opts: &ThermocoupleOptions) -> Result<ThermocoupleReading, String> {
    let u_inst = opts.instrument_uncertainty_c;
    if !(u_inst.is_finite() && u_inst >= 0.0) {
        return Err(
            "instrument_uncertainty_c must be finite and >= 0 (type-K standard limits are ±2.2 °C)"
                .into(),
        );
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', '!']))
        .collect();
    if lines.len() < 2 {
        return Err("thermocouple CSV: need a header row and data".into());
    }
    let header = split(lines[0]);
    let names: Vec<String> = header.iter().map(|h| clean(h)).collect();
    const TIME: [&str; 8] = [
        "t",
        "time",
        "time_s",
        "t_s",
        "seconds",
        "elapsed",
        "elapsed_s",
        "s",
    ];
    let ti = names
        .iter()
        .position(|h| TIME.contains(&h.as_str()))
        .ok_or_else(|| {
            format!(
                "thermocouple CSV: no time column in header ({}); name it time_s",
                header.join(", ")
            )
        })?;
    let ci = match &opts.column {
        Some(col) => header
            .iter()
            .position(|h| h == col || clean(h) == clean(col))
            .ok_or_else(|| {
                format!(
                    "thermocouple CSV: column {col:?} not in header ({})",
                    header.join(", ")
                )
            })?,
        None => {
            let temps: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(i, h)| {
                    *i != ti
                        && (h.contains("temp")
                            || h.contains("tc")
                            || h.contains('°')
                            || h.ends_with("_c")
                            || h.as_str() == "c"
                            || h.contains("celsius"))
                })
                .map(|(i, _)| i)
                .collect();
            if temps.len() != 1 {
                return Err(format!(
                    "thermocouple CSV: {} temperature column(s) in header ({}); pass `column`",
                    if temps.is_empty() { "no" } else { "several" },
                    header.join(", ")
                ));
            }
            temps[0]
        }
    };
    let mut data: Vec<(f64, f64)> = Vec::with_capacity(lines.len() - 1);
    for (i, line) in lines.iter().enumerate().skip(1) {
        let toks = split(line);
        let t = toks.get(ti).and_then(|s| s.parse::<f64>().ok());
        let v = toks.get(ci).and_then(|s| s.parse::<f64>().ok());
        match (t, v) {
            (Some(t), Some(v)) if t.is_finite() && v.is_finite() => data.push((t, v)),
            _ => {
                return Err(format!(
                    "thermocouple CSV row {}: non-numeric time or temperature",
                    i + 1
                ))
            }
        }
    }
    if data.len() < 5 {
        return Err("thermocouple CSV: need at least 5 samples".into());
    }
    let column = header[ci].to_string();
    if opts.reduce.as_deref() == Some("max") {
        let mx = data.iter().map(|d| d.1).fold(f64::NEG_INFINITY, f64::max);
        return Ok(ThermocoupleReading {
            value_c: mx,
            uncertainty_c: u_inst,
            spread_c: 0.0,
            samples: data.len(),
            window_s: None,
            drift_c: None,
            column,
        });
    }
    if let Some(r) = opts.reduce.as_deref() {
        if r != "steady" {
            return Err(format!("reduce must be \"steady\" or \"max\" (got {r:?})"));
        }
    }
    let (t0, t1) = (data[0].0, data[data.len() - 1].0);
    let window = opts.window_s.unwrap_or((t1 - t0) * 0.1);
    let win: Vec<(f64, f64)> = data
        .iter()
        .copied()
        .filter(|d| d.0 >= t1 - window)
        .collect();
    if win.len() < 5 {
        return Err(format!(
            "thermocouple CSV: only {} samples in the last {window} s — widen window_s",
            win.len()
        ));
    }
    let n = win.len() as f64;
    let mt = win.iter().map(|d| d.0).sum::<f64>() / n;
    let mv = win.iter().map(|d| d.1).sum::<f64>() / n;
    let sxy: f64 = win.iter().map(|d| (d.0 - mt) * (d.1 - mv)).sum();
    let sxx: f64 = win.iter().map(|d| (d.0 - mt).powi(2)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let span = win[win.len() - 1].0 - win[0].0;
    let drift = (slope * span).abs();
    let max_drift = opts.max_drift_c.unwrap_or(0.5);
    if drift > max_drift {
        return Err(format!(
            "thermocouple CSV: still drifting {drift:.2} °C over the last {span:.0} s (limit {max_drift}) — not at steady state; log longer or reduce:\"max\" for a peak claim"
        ));
    }
    let sd = (win.iter().map(|d| (d.1 - mv).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
    Ok(ThermocoupleReading {
        value_c: mv,
        uncertainty_c: u_inst.hypot(sd),
        spread_c: sd,
        samples: win.len(),
        window_s: Some(span),
        drift_c: Some(drift),
        column,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(u: f64) -> ThermocoupleOptions {
        ThermocoupleOptions {
            instrument_uncertainty_c: u,
            ..Default::default()
        }
    }

    fn settling_log() -> String {
        let mut s = String::from("time_s,tc1_c\n");
        for t in (0..=1800).step_by(10) {
            let t = t as f64;
            s.push_str(&format!(
                "{t},{:.3}\n",
                22.0 + 40.0 * (1.0 - (-t / 200.0).exp())
            ));
        }
        s
    }

    #[test]
    fn reads_the_plateau_and_the_peak() {
        let r = reduce_log(&settling_log(), &opts(1.1)).unwrap();
        assert!(r.value_c > 61.8 && r.value_c < 62.01, "{}", r.value_c);
        assert!(r.uncertainty_c >= 1.1);
        assert_eq!(r.column, "tc1_c");
        let peak = reduce_log(
            &settling_log(),
            &ThermocoupleOptions {
                reduce: Some("max".into()),
                ..opts(1.1)
            },
        )
        .unwrap();
        assert!(peak.value_c >= r.value_c);
    }

    #[test]
    fn refuses_a_log_that_is_still_heating() {
        let mut s = String::from("Time (s),Temp (C)\n");
        for i in 0..50 {
            s.push_str(&format!("{},{}\n", i * 10, 20 + i));
        }
        assert!(reduce_log(&s, &opts(1.0))
            .unwrap_err()
            .contains("steady state"));
    }

    #[test]
    fn refuses_malformed_logs() {
        assert!(reduce_log("a,b\n1,2", &opts(1.0)).is_err());
        assert!(reduce_log(&settling_log(), &opts(-1.0)).is_err());
        assert!(reduce_log(
            "time_s,t1_c,t2_c\n0,1,2\n1,1,2\n2,1,2\n3,1,2\n4,1,2",
            &opts(1.0)
        )
        .unwrap_err()
        .contains("several"));
        assert!(reduce_log("time_s,tc_c\n0,1\n1,x\n", &opts(1.0)).is_err());
    }
}
