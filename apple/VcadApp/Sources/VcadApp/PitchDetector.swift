import Accelerate
import Foundation

// Pitch detection for the Tune panel: find the fundamental of a struck bar.
//
// One analysis = Hann window → zero-pad ×2 → real FFT → local maxima of the
// log-power spectrum → the LOWEST peak within `relDb` of the strongest (a
// tuned bar's first overtone sits two octaves up and can out-ring the
// fundamental for a moment after the mallet leaves; a free-free bar has
// nothing below its fundamental) → parabolic interpolation on the log
// spectrum. At 48 kHz with 8192 samples the bin is 2.9 Hz and interpolation
// lands within ~0.1 cent of a pure tone.
//
// `StrikeDetector` turns a live stream into one estimate per strike: an
// onset is a block RMS jump over the tracked noise floor; analysis starts a
// short hold-off after it (skipping the mallet click) and a refractory
// period stops the ring-down re-triggering.

struct PitchEstimate: Equatable, Sendable {
    /// Fundamental, Hz.
    let hz: Double
    /// Peak level, dB (relative, uncalibrated).
    let db: Double
    /// Peak over the spectrum's median, dB.
    let snrDb: Double
    /// Other qualifying peaks above `hz` (ascending, Hz) — when the lowest
    /// peak is room noise, a caller that knows the targets can pick one.
    var alternates: [Double] = []
}

enum PitchDetector {
    /// Estimate the fundamental of `samples`, or nil when nothing stands out.
    static func fundamental(_ samples: [Float], sampleRate: Double,
                            minHz: Double = 150, maxHz: Double = 9000,
                            relDb: Double = 20, minSnrDb: Double = 18) -> PitchEstimate? {
        let count = samples.count
        guard count >= 256, sampleRate > 0 else { return nil }
        var n = 1
        while n < count { n <<= 1 }
        n = min(n * 2, 1 << 17)
        let log2n = vDSP_Length(n.trailingZeroBitCount)

        // Hann window, zero-padded.
        var window = [Float](repeating: 0, count: count)
        vDSP_hann_window(&window, vDSP_Length(count), Int32(vDSP_HANN_NORM))
        var buf = [Float](repeating: 0, count: n)
        vDSP_vmul(samples, 1, window, 1, &buf, 1, vDSP_Length(count))

        guard let setup = vDSP_create_fftsetup(log2n, FFTRadix(kFFTRadix2)) else { return nil }
        defer { vDSP_destroy_fftsetup(setup) }
        let half = n / 2
        var re = [Float](repeating: 0, count: half)
        var im = [Float](repeating: 0, count: half)
        var power = [Float](repeating: 0, count: half)
        re.withUnsafeMutableBufferPointer { rp in
            im.withUnsafeMutableBufferPointer { ip in
                var split = DSPSplitComplex(realp: rp.baseAddress!, imagp: ip.baseAddress!)
                buf.withUnsafeBufferPointer { bp in
                    bp.baseAddress!.withMemoryRebound(to: DSPComplex.self, capacity: half) {
                        vDSP_ctoz($0, 2, &split, 1, vDSP_Length(half))
                    }
                }
                vDSP_fft_zrip(setup, &split, 1, log2n, FFTDirection(FFT_FORWARD))
                split.imagp[0] = 0 // packed Nyquist; DC/Nyquist are outside the band anyway
                vDSP_zvmags(&split, 1, &power, 1, vDSP_Length(half))
            }
        }
        let db = power.map { 10 * log10(Double($0) + 1e-30) }
        let binHz = sampleRate / Double(n)
        let k0 = max(2, Int(minHz / binHz))
        let k1 = min(half - 2, Int(maxHz / binHz))
        guard k1 > k0 else { return nil }

        var peaks: [Int] = []
        var top = -Double.infinity
        for k in k0...k1 where db[k] > db[k - 1] && db[k] >= db[k + 1] {
            peaks.append(k)
            top = max(top, db[k])
        }
        guard !peaks.isEmpty else { return nil }
        let band = Array(db[k0...k1]).sorted()
        let median = band[band.count / 2]
        guard top - median >= minSnrDb else { return nil }
        let qualifying = peaks.filter { db[$0] >= top - relDb && db[$0] - median >= minSnrDb }
        guard let k = qualifying.first else { return nil }

        func refine(_ k: Int) -> (hz: Double, db: Double) {
            let (a, b, c) = (db[k - 1], db[k], db[k + 1])
            let denom = a - 2 * b + c
            let p = denom == 0 ? 0 : 0.5 * (a - c) / denom
            return ((Double(k) + p) * binHz, b - 0.25 * (a - c) * p)
        }
        let f = refine(k)
        return PitchEstimate(hz: f.hz, db: f.db, snrDb: db[k] - median,
                             alternates: qualifying.dropFirst().prefix(8).map { refine($0).hz })
    }

    /// Strongest peak (and its SNR) for debug logs.
    static func debugPeaks(_ samples: [Float], sampleRate: Double) -> String {
        guard let e = fundamental(samples, sampleRate: sampleRate, relDb: 0, minSnrDb: 0) else { return "none" }
        return String(format: "strongest %.1f Hz snr %.0f", e.hz, e.snrDb)
    }

    /// Nearest 12-TET note (A4 = 440) and the offset in cents.
    static func nearestNote(_ hz: Double) -> (name: String, cents: Double) {
        let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
        let midi = Int((69 + 12 * log2(hz / 440)).rounded())
        let ref = 440 * pow(2, Double(midi - 69) / 12)
        let octave = Int((Double(midi) / 12).rounded(.down)) - 1
        return ("\(names[((midi % 12) + 12) % 12])\(octave)", cents(hz, ref))
    }

    static func cents(_ a: Double, _ b: Double) -> Double { 1200 * log2(a / b) }
}

/// Live stream → one pitch estimate per strike. Not thread-safe: feed it from
/// one thread (the audio tap).
final class StrikeDetector {
    /// `VCAD_TUNE_DEBUG=1`: log onsets and estimates to stderr.
    static let debug = ProcessInfo.processInfo.environment["VCAD_TUNE_DEBUG"] != nil
    let sampleRate: Double
    let windowSamples: Int
    private let holdoffSamples: Int
    private let refractorySamples: Int
    private let block = 256
    private let onsetRatio: Float
    private let minRms: Float

    private var floorRms: Float = 0
    private var pending: [Float] = []   // partial block
    private var capture: [Float]?
    private var skip = 0                // hold-off samples still to drop
    private var quiet = 0               // refractory samples remaining
    private var prevRms: Float = 0      // last block's RMS (onset = a jump)

    init(sampleRate: Double, windowSeconds: Double = 0.17, holdoffSeconds: Double = 0.02,
         refractorySeconds: Double = 0.3, onsetRatio: Float = 6, minRms: Float = 0.002) {
        self.sampleRate = sampleRate
        var w = 1
        while w < Int(windowSeconds * sampleRate) { w <<= 1 }
        windowSamples = w
        holdoffSamples = Int(holdoffSeconds * sampleRate)
        refractorySamples = Int(refractorySeconds * sampleRate)
        self.onsetRatio = onsetRatio
        self.minRms = minRms
    }

    /// Feed samples; returns any strikes completed in this chunk.
    func process(_ samples: UnsafeBufferPointer<Float>) -> [PitchEstimate] {
        var out: [PitchEstimate] = []
        pending.append(contentsOf: samples)
        var i = 0
        while i + block <= pending.count {
            let chunk = pending[i..<(i + block)]
            i += block
            if var cap = capture {
                if skip > 0 {
                    let drop = min(skip, block)
                    skip -= drop
                    cap.append(contentsOf: chunk.dropFirst(drop))
                } else {
                    cap.append(contentsOf: chunk)
                }
                if cap.count >= windowSamples {
                    let window = Array(cap.prefix(windowSamples))
                    capture = nil
                    quiet = refractorySamples
                    let e = PitchDetector.fundamental(window, sampleRate: sampleRate)
                    if Self.debug {
                        let peaks = PitchDetector.debugPeaks(window, sampleRate: sampleRate)
                        FileHandle.standardError.write(Data("[tune] strike → \(e.map { String(format: "%.2f Hz snr %.0f", $0.hz, $0.snrDb) } ?? "rejected") peaks \(peaks)\n".utf8))
                    }
                    if let e { out.append(e) }
                } else {
                    capture = cap
                }
                continue
            }
            var rms: Float = 0
            chunk.withUnsafeBufferPointer { vDSP_rmsqv($0.baseAddress!, 1, &rms, vDSP_Length(block)) }
            let previous = prevRms
            prevRms = rms
            if quiet > 0 {
                quiet -= block
                continue
            }
            // A strike is loud over the floor AND a sudden jump: a decaying
            // ring-down is loud but never jumps, so it can't re-trigger.
            if Self.debug && rms > max(minRms, floorRms * onsetRatio) {
                FileHandle.standardError.write(Data(String(format: "[tune] loud rms %.4f floor %.4f prev %.4f\n", rms, floorRms, previous).utf8))
            }
            if floorRms > 0 && rms > max(minRms, floorRms * onsetRatio) && rms > 2.5 * previous {
                capture = []
                skip = holdoffSamples
            } else {
                let r = max(rms, 1e-5) // digital silence still seeds a floor
                floorRms = floorRms == 0 ? r : 0.95 * floorRms + 0.05 * r
            }
        }
        pending.removeFirst(i)
        return out
    }
}
