//! Strict JSON readers that keep nothing: `serde` visitors that walk a whole
//! document through `serde_json`'s `deserialize_any` and pick out the few
//! fields the latency probe needs.
//!
//! Why `deserialize_any` and not `serde::de::IgnoredAny`: `serde_json` skips
//! an ignored value with a cheaper scanner that does not range-check numbers,
//! pair surrogates or count nesting depth, so it accepts text that building a
//! `serde_json::Value` rejects. Every value here, wanted or not, goes through
//! the same validating path as `Value`, so these readers accept and reject
//! exactly the same text with exactly the same error, without building the
//! tree. Duplicate keys resolve as in `Value`: the last one wins.
//!
//! Heap use: none while reading. `serde_json` unescapes a string that
//! contains a backslash into a scratch buffer it owns, so a document with
//! escaped strings costs one growing allocation per parse; hl-node output and
//! the public `l2Book` / `trades` frames carry no escapes.

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::{parse_naive_utc_ms, Observation, ParseError, Source};

/// Reads and validates the rest of an array, keeping nothing.
fn skip_seq<'de, A: SeqAccess<'de>>(seq: &mut A) -> Result<(), A::Error> {
    while seq.next_element_seed(Skip)?.is_some() {}
    Ok(())
}

/// Reads and validates the rest of an object, keeping nothing.
fn skip_map<'de, A: MapAccess<'de>>(map: &mut A) -> Result<(), A::Error> {
    while map.next_key_seed(Key(&[]))?.is_some() {
        map.next_value_seed(Skip)?;
    }
    Ok(())
}

/// Visitor methods for JSON kinds a reader does not care about: validate,
/// then answer with the reader's "absent" value (its `Default`).
macro_rules! absent_for {
    () => {};
    (bool $($rest:ident)*) => {
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (null $($rest:ident)*) => {
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (int $($rest:ident)*) => {
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (float $($rest:ident)*) => {
        // `Visitor::visit_f64` is how serde hands over a JSON number with a
        // fraction, an exponent or more digits than a `u64` holds; without
        // this method such a number would be a type error instead of "not a
        // u64". The value is dropped unread: no float is stored or computed.
        #[expect(
            clippy::disallowed_types,
            reason = "serde's Visitor::visit_f64 signature; the value is discarded"
        )]
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (str $($rest:ident)*) => {
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (seq $($rest:ident)*) => {
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            skip_seq(&mut seq)?;
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
    (map $($rest:ident)*) => {
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            skip_map(&mut map)?;
            Ok(Default::default())
        }
        absent_for!($($rest)*);
    };
}

/// A reader that is its own `Visitor`, driven through `deserialize_any`.
macro_rules! seed_via_any {
    ($ty:ty) => {
        impl<'de> DeserializeSeed<'de> for $ty {
            type Value = <Self as Visitor<'de>>::Value;
            fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                d.deserialize_any(self)
            }
        }
    };
}

/// Any value, validated and dropped.
struct Skip;
seed_via_any!(Skip);
impl<'de> Visitor<'de> for Skip {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    absent_for!(bool null int float str seq map);
}

/// An object key: its index in the wanted names, or the names' length for any
/// other key. Compared after unescaping, as `Value` does.
struct Key(&'static [&'static str]);
seed_via_any!(Key);
impl Visitor<'_> for Key {
    type Value = usize;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object key")
    }
    fn visit_str<E>(self, s: &str) -> Result<usize, E> {
        Ok(self.0.iter().position(|k| *k == s).unwrap_or(self.0.len()))
    }
}

/// `Value::as_u64`: a non-negative integer that fits a `u64`.
struct U64Field;
seed_via_any!(U64Field);
impl<'de> Visitor<'de> for U64Field {
    type Value = Option<u64>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Some(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(u64::try_from(v).ok())
    }
    absent_for!(bool null float str seq map);
}

/// A string parsed as a node timestamp while `serde_json` still holds it, so
/// an escaped string needs no owned copy. `None`: not a string.
struct TimestampField;
seed_via_any!(TimestampField);
impl<'de> Visitor<'de> for TimestampField {
    type Value = Option<Result<u64, ParseError>>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E> {
        Ok(Some(parse_naive_utc_ms(s)))
    }
    absent_for!(bool null int float seq map);
}

/// What a node line says about its block.
#[derive(Default)]
pub(crate) struct NodeFields {
    /// Last `block_time` that is a string, parsed. `None`: missing or not a
    /// string.
    pub(crate) block_time: Option<Result<u64, ParseError>>,
    /// Last `block_number`, when it is a u64.
    pub(crate) block_number: Option<u64>,
}

/// A whole node line. Anything but an object has neither field.
pub(crate) struct NodeLine;
seed_via_any!(NodeLine);
impl<'de> Visitor<'de> for NodeLine {
    type Value = NodeFields;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<NodeFields, A::Error> {
        let mut out = NodeFields::default();
        while let Some(key) = map.next_key_seed(Key(&["block_time", "block_number"]))? {
            match key {
                0 => out.block_time = map.next_value_seed(TimestampField)?,
                1 => out.block_number = map.next_value_seed(U64Field)?,
                _ => map.next_value_seed(Skip)?,
            }
        }
        Ok(out)
    }
    absent_for!(bool null int float str seq);
}

/// The `channel` of a public `WebSocket` frame.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    /// `"l2Book"`.
    L2Book,
    /// `"trades"`.
    Trades,
    /// Missing, not a string, or a channel that carries no block time.
    #[default]
    Other,
}

struct ChannelField;
seed_via_any!(ChannelField);
impl<'de> Visitor<'de> for ChannelField {
    type Value = Channel;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_str<E>(self, s: &str) -> Result<Channel, E> {
        Ok(match s {
            "l2Book" => Channel::L2Book,
            "trades" => Channel::Trades,
            _ => Channel::Other,
        })
    }
    absent_for!(bool null int float seq map);
}

/// `value.get("time").and_then(Value::as_u64)`: the last `time` of an object,
/// when it is a u64. `None` for anything else.
fn object_time<'de, A: MapAccess<'de>>(map: &mut A) -> Result<Option<u64>, A::Error> {
    let mut time = None;
    while let Some(key) = map.next_key_seed(Key(&["time"]))? {
        if key == 0 {
            time = map.next_value_seed(U64Field)?;
        } else {
            map.next_value_seed(Skip)?;
        }
    }
    Ok(time)
}

struct ObjectTime;
seed_via_any!(ObjectTime);
impl<'de> Visitor<'de> for ObjectTime {
    type Value = Option<u64>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<u64>, A::Error> {
        object_time(&mut map)
    }
    absent_for!(bool null int float str seq);
}

/// Both readings of a frame's `data`, taken in one pass because `channel`
/// may come after it.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Data {
    /// An object, with its `time` when that is a u64 (the `l2Book` reading).
    Object(Option<u64>),
    /// An array (the `trades` reading). `all_timed` is false when an element
    /// had no u64 `time`. The distinct times, up to the first such element,
    /// are in the output buffer.
    Array {
        /// Every element was an object with a u64 `time`.
        all_timed: bool,
    },
    /// Any other JSON value.
    #[default]
    Other,
}

struct DataField<'a> {
    out: &'a mut Vec<Observation>,
    recv_ns: u64,
}
impl<'de> DeserializeSeed<'de> for DataField<'_> {
    type Value = Data;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Data, D::Error> {
        // A repeated `data` key replaces the earlier one.
        self.out.clear();
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for DataField<'_> {
    type Value = Data;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Data, A::Error> {
        object_time(&mut map).map(Data::Object)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Data, A::Error> {
        let mut all_timed = true;
        while let Some(time) = seq.next_element_seed(ObjectTime)? {
            match time {
                Some(t) if all_timed => {
                    if !self.out.iter().any(|o| o.block_time_ms == t) {
                        self.out.push(Observation {
                            source: Source::PublicWs,
                            block_time_ms: t,
                            block_number: None,
                            recv_ns: self.recv_ns,
                            file_date: None,
                            file_hour: None,
                            line_offset: None,
                        });
                    }
                }
                Some(_) => {}
                None => all_timed = false,
            }
        }
        Ok(Data::Array { all_timed })
    }
    absent_for!(bool null int float str);
}

/// A whole public `WebSocket` frame: its last `channel` and last `data`
/// (`None`: no `data` key). Anything but an object has neither.
pub(crate) struct WsFrame<'a> {
    /// Receives the `trades` reading of `data`; see [`Data::Array`].
    pub(crate) out: &'a mut Vec<Observation>,
    /// Stamp for the observations pushed to `out`.
    pub(crate) recv_ns: u64,
}
impl<'de> DeserializeSeed<'de> for WsFrame<'_> {
    type Value = (Channel, Option<Data>);
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for WsFrame<'_> {
    type Value = (Channel, Option<Data>);
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut channel, mut data) = (Channel::Other, None);
        while let Some(key) = map.next_key_seed(Key(&["channel", "data"]))? {
            match key {
                0 => channel = map.next_value_seed(ChannelField)?,
                1 => {
                    data = Some(map.next_value_seed(DataField {
                        out: &mut *self.out,
                        recv_ns: self.recv_ns,
                    })?);
                }
                _ => map.next_value_seed(Skip)?,
            }
        }
        Ok((channel, data))
    }
    absent_for!(bool null int float str seq);
}
