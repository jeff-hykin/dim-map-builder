//! Streams stamped on another clock. A device that stamps its messages with its own clock (the header stamps) can sit
//! seconds or minutes from the clock the recorder logs them on. When tf was written on the log clock and the scans
//! weren't, the scans' stamps are moved onto it by this offset, keeping their own spacing (the build tries both and
//! keeps whichever tf covers). Nothing here knows any sensor.

/// Offsets smaller than this are delivery latency, not another clock.
pub const TOLERANCE_SECONDS: f64 = 0.5;

/// What to add to a stream's header stamps to put them on the log clock, from (log time, header stamp) samples: the
/// smallest log - stamp (delivery only ever adds latency), when that's beyond TOLERANCE_SECONDS; else 0. Unstamped
/// samples (stamp 0) don't count.
pub fn offset(samples: impl IntoIterator<Item = (f64, f64)>) -> f64 {
    let smallest = samples.into_iter().filter(|(_, stamp)| *stamp > 0.0).map(|(log, stamp)| log - stamp).fold(f64::INFINITY, f64::min);
    if smallest.is_finite() && smallest.abs() > TOLERANCE_SECONDS {
        smallest
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_minutes_behind_is_shifted_and_latency_is_not() {
        // a device clock 143 s behind, delivered 10 to 40 ms late
        let behind: Vec<(f64, f64)> = (0..50).map(|i| (1000.0 + i as f64 * 0.1, 857.0 + i as f64 * 0.1 - 0.01 - (i % 4) as f64 * 0.01)).collect();
        assert!((offset(behind) - 143.0).abs() < 0.02);
        let late: Vec<(f64, f64)> = (0..50).map(|i| (1000.0 + i as f64 * 0.1, 1000.0 + i as f64 * 0.1 - 0.04)).collect();
        assert_eq!(offset(late), 0.0);
        // stamps ahead of the log clock shift back
        assert!((offset([(10.0, 70.0), (11.0, 71.0)]) + 60.0).abs() < 1e-9);
        // unstamped messages say nothing
        assert_eq!(offset([(10.0, 0.0)]), 0.0);
    }
}
