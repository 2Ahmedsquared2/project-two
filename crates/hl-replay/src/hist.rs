//! Latency histograms: HDR recording, integer nearest-rank percentiles.

use std::io::{self, Read, Write};

use hdrhistogram::serialization::{Deserializer, Serializer, V2Serializer};
use hdrhistogram::Histogram;

use crate::ReplayError;

/// The largest latency a histogram tracks, in nanoseconds (60 s). Larger
/// values are recorded as this one rather than dropped.
pub const HIST_MAX_NS: u64 = 60_000_000_000;

/// Significant decimal digits kept by every histogram.
const SIGFIG: u8 = 3;

/// The numbers a report prints for one histogram. There is no mean: latency
/// is a distribution and the slow end is the point.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Samples recorded.
    pub count: u64,
    /// Median, ns.
    pub p50: u64,
    /// 90th percentile, ns.
    pub p90: u64,
    /// 99th percentile, ns.
    pub p99: u64,
    /// 99.9th percentile, ns.
    pub p999: u64,
    /// 99.99th percentile, ns.
    pub p9999: u64,
    /// Largest sample, ns.
    pub max: u64,
}

/// An HDR histogram of nanosecond latencies, 1 ns to [`HIST_MAX_NS`] at three
/// significant digits. Recording never allocates. Percentiles are computed
/// here with integers (nearest rank over the recorded buckets) because this
/// workspace bans floats; `hdrhistogram`'s own quantile calls take `f64`.
#[derive(Debug, Clone)]
pub struct LatencyHist {
    h: Histogram<u64>,
}

impl Default for LatencyHist {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyHist {
    /// An empty histogram. Allocates its counts once, here.
    ///
    /// # Panics
    ///
    /// Never in practice: the bounds are compile-time constants that
    /// `hdrhistogram` accepts.
    #[must_use]
    pub fn new() -> Self {
        match Histogram::new_with_bounds(1, HIST_MAX_NS, SIGFIG) {
            Ok(h) => Self { h },
            Err(e) => unreachable!("constant histogram bounds rejected: {e}"),
        }
    }

    /// Records one latency. Zero counts as 1 ns and anything above
    /// [`HIST_MAX_NS`] as [`HIST_MAX_NS`], so no sample is ever lost.
    #[inline]
    pub fn record(&mut self, ns: u64) {
        self.h.saturating_record(ns.clamp(1, HIST_MAX_NS));
    }

    /// Samples recorded.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.h.len()
    }

    /// Adds another histogram's samples to this one. This is how rounds are
    /// combined: percentiles are never averaged.
    pub fn merge(&mut self, other: &Self) {
        // Both sides have the same bounds, so every bucket has a home; a
        // deserialised histogram with wider bounds saturates like `record`.
        if self.h.add(&other.h).is_err() {
            for v in other.h.iter_recorded() {
                self.h.saturating_record_n(
                    v.value_iterated_to().min(HIST_MAX_NS),
                    v.count_at_value(),
                );
            }
        }
    }

    /// The nearest-rank value at `q` parts per 100,000 (50,000 is the median,
    /// 99,900 is p99.9). Returns 0 for an empty histogram.
    #[must_use]
    pub fn value_at_per_100k(&self, q: u32) -> u64 {
        let n = u128::from(self.h.len());
        if n == 0 {
            return 0;
        }
        let q = u128::from(q.min(100_000));
        // ceil(q * n / 100000), at least 1.
        let rank = (q * n).div_ceil(100_000).max(1);
        let mut seen: u128 = 0;
        for v in self.h.iter_recorded() {
            seen += u128::from(v.count_at_value());
            if seen >= rank {
                return v.value_iterated_to().min(self.h.max());
            }
        }
        self.h.max()
    }

    /// Largest sample (to the histogram's precision), 0 when empty.
    #[must_use]
    pub fn max(&self) -> u64 {
        if self.h.is_empty() {
            0
        } else {
            self.h.max()
        }
    }

    /// Count, p50, p90, p99, p99.9, p99.99 and max.
    #[must_use]
    pub fn summary(&self) -> Summary {
        Summary {
            count: self.count(),
            p50: self.value_at_per_100k(50_000),
            p90: self.value_at_per_100k(90_000),
            p99: self.value_at_per_100k(99_000),
            p999: self.value_at_per_100k(99_900),
            p9999: self.value_at_per_100k(99_990),
            max: self.max(),
        }
    }

    /// Writes the histogram in the standard HDR V2 binary serialisation,
    /// readable by any `HdrHistogram` implementation.
    pub fn write_v2<W: Write>(&self, w: &mut W) -> Result<(), ReplayError> {
        V2Serializer::new()
            .serialize(&self.h, w)
            .map(|_| ())
            .map_err(|e| ReplayError::Histogram(e.to_string()))
    }

    /// Reads a histogram written by [`LatencyHist::write_v2`].
    pub fn read_v2<R: Read>(r: &mut R) -> Result<Self, ReplayError> {
        Deserializer::new()
            .deserialize(r)
            .map(|h| Self { h })
            .map_err(|e| ReplayError::Histogram(format!("{e:?}")))
    }

    /// Writes every non-empty bucket as `value_ns count`, ascending, under a
    /// `# value_ns count` header: a lossless text dump for tools that do not
    /// read HDR files.
    pub fn write_buckets<W: Write>(&self, w: &mut W) -> io::Result<()> {
        writeln!(w, "# value_ns count")?;
        for v in self.h.iter_recorded() {
            writeln!(w, "{} {}", v.value_iterated_to(), v.count_at_value())?;
        }
        Ok(())
    }
}
