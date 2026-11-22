//! Bit-serial UART (8N1) models and the FT231XS USB-UART bridge.
//!
//! Bit timing follows the sys clock: one bit is `clk_hz / baud` cycles,
//! kept fractional so long frames do not drift (115200 baud at 100 MHz is
//! 868.06 cycles per bit).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::rc::Rc;

use super::PinModel;
use vcad_ecad_schematic::drive::PinDir;

/// Shared console sink: every byte the host side receives.
#[derive(Debug, Clone, Default)]
pub struct Console {
    inner: Rc<RefCell<Vec<u8>>>,
}

impl Console {
    /// All bytes received so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.inner.borrow().clone()
    }

    /// Number of bytes received.
    pub fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    /// Whether nothing was received.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_empty()
    }

    /// Bytes from `from` on.
    pub fn since(&self, from: usize) -> Vec<u8> {
        self.inner.borrow()[from..].to_vec()
    }

    fn push(&self, b: u8) {
        self.inner.borrow_mut().push(b);
    }
}

/// Bit period in 1/2^16 cycle units.
fn period_fx(clk_hz: u64, baud: u64) -> u64 {
    (clk_hz << 16) / baud
}

/// 8N1 receiver: samples mid-bit after a falling start edge.
#[derive(Debug, Clone)]
pub struct UartRx {
    period: u64,
    now: u64,
    level: bool,
    prev: bool,
    /// Next sample time (fixed point) and bit index (0 = start, 1..=8 data,
    /// 9 = stop), when inside a frame.
    frame: Option<(u64, u8, u8)>,
    /// Frames whose stop bit was low.
    pub framing_errors: u64,
}

impl UartRx {
    /// Receiver for `baud` on a `clk_hz` sys clock.
    pub fn new(clk_hz: u64, baud: u64) -> Self {
        Self {
            period: period_fx(clk_hz, baud),
            now: 0,
            level: true,
            prev: true,
            frame: None,
            framing_errors: 0,
        }
    }

    /// Present the line level for this cycle.
    pub fn sense(&mut self, level: bool) {
        self.level = level;
    }

    /// Advance one cycle; returns a byte when a frame completes.
    pub fn tick(&mut self) -> Option<u8> {
        let t = self.now;
        self.now += 1 << 16;
        let level = self.level;
        let falling = self.prev && !level;
        self.prev = level;
        match self.frame {
            None => {
                if falling {
                    self.frame = Some((t + self.period / 2, 0, 0));
                }
                None
            }
            Some((at, bit, data)) => {
                if t < at {
                    return None;
                }
                let next = at + self.period;
                match bit {
                    0 => {
                        // A glitch, not a start bit.
                        self.frame = if level { None } else { Some((next, 1, 0)) };
                        None
                    }
                    1..=8 => {
                        let d = data | (u8::from(level) << (bit - 1));
                        self.frame = Some((next, bit + 1, d));
                        None
                    }
                    _ => {
                        self.frame = None;
                        if level {
                            Some(data)
                        } else {
                            self.framing_errors += 1;
                            None
                        }
                    }
                }
            }
        }
    }
}

/// 8N1 transmitter; idles high.
#[derive(Debug, Clone)]
pub struct UartTx {
    period: u64,
    now: u64,
    queue: VecDeque<u8>,
    /// (frame start time, current byte).
    frame: Option<(u64, u8)>,
    level: bool,
}

impl UartTx {
    /// Transmitter for `baud` on a `clk_hz` sys clock.
    pub fn new(clk_hz: u64, baud: u64) -> Self {
        Self {
            period: period_fx(clk_hz, baud),
            now: 0,
            queue: VecDeque::new(),
            frame: None,
            level: true,
        }
    }

    /// Queue bytes to send.
    pub fn send(&mut self, bytes: &[u8]) {
        self.queue.extend(bytes.iter().copied());
    }

    /// Whether everything queued has gone out.
    pub fn idle(&self) -> bool {
        self.frame.is_none() && self.queue.is_empty()
    }

    /// Current line level.
    pub fn level(&self) -> bool {
        self.level
    }

    /// Advance one cycle.
    pub fn tick(&mut self) {
        let t = self.now;
        self.now += 1 << 16;
        if let Some((start, _)) = self.frame {
            if (t - start) / self.period >= 10 {
                self.frame = None;
            }
        }
        if self.frame.is_none() {
            if let Some(b) = self.queue.pop_front() {
                self.frame = Some((t, b));
            }
        }
        self.level = match self.frame {
            None => true,
            Some((start, b)) => match (t - start) / self.period {
                0 => false,
                k @ 1..=8 => (b >> (k - 1)) & 1 == 1,
                _ => true,
            },
        };
    }
}

/// FTDI FT231XS USB-UART: `RXD` (in) decodes to the console, `TXD` (out)
/// sends queued host keystrokes. Other pins (RTS/DTR/…) are not modelled.
#[derive(Debug, Clone)]
pub struct Ft231x {
    rx: UartRx,
    tx: UartTx,
    console: Console,
    echo: bool,
}

impl Ft231x {
    /// Bridge at `baud` on a `clk_hz` sys clock, feeding `console`; with
    /// `echo`, received bytes are also written to stdout as they arrive.
    pub fn new(clk_hz: u64, baud: u64, console: Console, echo: bool) -> Self {
        Self {
            rx: UartRx::new(clk_hz, baud),
            tx: UartTx::new(clk_hz, baud),
            console,
            echo,
        }
    }

    /// Queue host-side bytes onto TXD.
    pub fn send(&mut self, bytes: &[u8]) {
        self.tx.send(bytes);
    }

    /// Framing errors seen on RXD.
    pub fn framing_errors(&self) -> u64 {
        self.rx.framing_errors
    }
}

impl PinModel for Ft231x {
    fn pins(&self) -> Vec<(String, PinDir)> {
        vec![("TXD".into(), PinDir::Out), ("RXD".into(), PinDir::In)]
    }

    fn drive(&self, pin: usize) -> bool {
        pin != 0 || self.tx.level()
    }

    fn sense(&mut self, pin: usize, level: bool) {
        if pin == 1 {
            self.rx.sense(level);
        }
    }

    fn tick(&mut self) {
        self.tx.tick();
        if let Some(b) = self.rx.tick() {
            self.console.push(b);
            if self.echo {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&[b]);
                let _ = out.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback(clk: u64, baud: u64, rx_baud: u64, msg: &[u8]) -> (Vec<u8>, u64) {
        let mut tx = UartTx::new(clk, baud);
        let mut rx = UartRx::new(clk, rx_baud);
        tx.send(msg);
        let mut got = Vec::new();
        for _ in 0..(clk / baud + 1) * 10 * (msg.len() as u64 + 2) {
            tx.tick();
            rx.sense(tx.level());
            if let Some(b) = rx.tick() {
                got.push(b);
            }
        }
        (got, rx.framing_errors)
    }

    #[test]
    fn loopback_at_fractional_divisor() {
        // 115200 @ 100 MHz = 868.06 cycles/bit.
        let msg: Vec<u8> = (0..=255u8).collect();
        let (got, fe) = loopback(100_000_000, 115_200, 115_200, &msg);
        assert_eq!(got, msg);
        assert_eq!(fe, 0);
    }

    #[test]
    fn tolerates_two_percent_baud_mismatch() {
        let (got, _) = loopback(1_000_000, 50_000, 51_000, b"hello");
        assert_eq!(got, b"hello");
    }

    #[test]
    fn silent_line_gives_nothing() {
        let mut rx = UartRx::new(1_000_000, 50_000);
        for _ in 0..100_000 {
            rx.sense(true);
            assert!(rx.tick().is_none());
        }
    }
}
