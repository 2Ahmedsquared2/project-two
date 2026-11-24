//! A fixed-size log-linear histogram of nanosecond lags: values below 8 are
//! exact; above, each power of two is split into 8 buckets, so a bucket's
//! width is at most 1/8 of its lower bound. 496 `u64` counters, allocated
//! once; [`LagHistogram::record`] never allocates.

use serde::ser::{SerializeStruct as _, Serializer};
use serde::Serialize;

const SUB_BITS: u32 = 3;
const SUB: u64 = 1 << SUB_BITS;
/// Number of buckets: 8 exact + 61 octaves (2^3 .. 2^63) x 8.
pub(crate) const BUCKETS: usize = 8 + 61 * 8;

/// Bucket index of `v`.
#[must_use]
pub(crate) fn bucket(v: u64) -> usize {
    if v < SUB {
        return usize::try_from(v).unwrap_or(0);
    }
    let msb = v.ilog2(); // 3..=63
    let sub = (v >> (msb - SUB_BITS)) & (SUB - 1);
    let idx = SUB + u64::from(msb - SUB_BITS) * SUB + sub;
    usize::try_from(idx).unwrap_or(BUCKETS - 1)
}

/// Smallest value that falls in bucket `idx`.
#[must_use]
pub(crate) fn bucket_low(idx: usize) -> u64 {
    let i = u64::try_from(idx).unwrap_or(0);
    if i < SUB {
        return i;
    }
    let octave = (i - SUB) / SUB; // msb - 3
    let sub = (i - SUB) % SUB;
    (SUB + sub) << octave
}

/// Lag histogram, nanoseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LagHistogram {
    counts: Box<[u64; BUCKETS]>,
    total: u64,
    max: u64,
}

impl Default for LagHistogram {
    fn default() -> Self {
        Self {
            counts: Box::new([0; BUCKETS]),
            total: 0,
            max: 0,
        }
    }
}

impl LagHistogram {
    /// Counts one value.
    pub fn record(&mut self, v: u64) {
        self.counts[bucket(v)] += 1;
        self.total += 1;
        self.max = self.max.max(v);
    }

    /// Values recorded.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.total
    }

    /// Largest value recorded (exact).
    #[must_use]
    pub fn max(&self) -> u64 {
        self.max
    }

    /// Lower bound of the bucket holding the `per_mille`/1000 quantile
    /// (nearest rank), or `None` when empty. Within 12.5% below the true
    /// value.
    #[must_use]
    pub fn quantile_low(&self, per_mille: u32) -> Option<u64> {
        if self.total == 0 {
            return None;
        }
        let rank = (self.total * u64::from(per_mille.min(1000)))
            .div_ceil(1000)
            .max(1);
        let mut seen = 0;
        for (i, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Some(bucket_low(i));
            }
        }
        None
    }
}

impl Serialize for LagHistogram {
    /// `{count, max, p50, p90, p99, buckets: [[low, count], ...]}` with only
    /// non-empty buckets: the raw data behind every percentile.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let buckets: Vec<(u64, u64)> = self
            .counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (bucket_low(i), *c))
            .collect();
        let mut st = s.serialize_struct("LagHistogram", 6)?;
        st.serialize_field("count", &self.total)?;
        st.serialize_field("max", &self.max)?;
        st.serialize_field("p50_low", &self.quantile_low(500))?;
        st.serialize_field("p90_low", &self.quantile_low(900))?;
        st.serialize_field("p99_low", &self.quantile_low(990))?;
        st.serialize_field("buckets", &buckets)?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_bounds_contain_value() {
        let probe = [
            0u64,
            1,
            7,
            8,
            9,
            15,
            16,
            17,
            1000,
            123_456_789,
            u64::MAX / 3,
            u64::MAX,
        ];
        for v in probe {
            let b = bucket(v);
            assert!(b < BUCKETS, "{v}");
            assert!(bucket_low(b) <= v, "{v}");
            if b + 1 < BUCKETS {
                assert!(v < bucket_low(b + 1), "{v}");
            }
        }
        assert_eq!(bucket(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn buckets_are_monotone() {
        for i in 1..BUCKETS {
            assert!(bucket_low(i - 1) < bucket_low(i));
            assert_eq!(bucket(bucket_low(i)), i);
        }
    }

    #[test]
    fn quantiles_known_answer() {
        let mut h = LagHistogram::default();
        for v in 1..=100u64 {
            h.record(v * 1000);
        }
        // True p50 = 50_000; bucket of 50_000 is [49_152, 53_248).
        assert_eq!(h.quantile_low(500), Some(49_152));
        assert_eq!(h.max(), 100_000);
        assert_eq!(h.count(), 100);
        assert_eq!(LagHistogram::default().quantile_low(500), None);
    }
}
