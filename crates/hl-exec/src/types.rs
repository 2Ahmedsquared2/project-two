//! Plain hot-path types: no serde, no heap. The replay driver (lane 3)
//! decodes `replica_cmds` into these and maps [`BlockOutcome`] back to native
//! response JSON for comparison.

/// A price in integer ticks of the asset's price grid (`price = ticks / 10^max_price_decimals`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Px(pub u64);

/// A size in integer lots of the asset's size grid (`size = lots / 10^sz_decimals`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Sz(pub u64);

/// An order id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Oid(pub u64);

/// A client order id (16 bytes, `0x` + 32 hex digits on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Cloid(pub u128);

/// An account address (20 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct User(pub [u8; 20]);

/// Hyperliquid asset index: perp = position in `meta.universe`, spot = 10000 + spot index.
pub type Asset = u32;

/// Side of the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// Buy.
    Bid,
    /// Sell.
    Ask,
}

impl Side {
    /// The other side.
    #[must_use]
    pub fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// Time in force of a limit order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tif {
    /// Good till cancelled: match, rest the remainder.
    Gtc,
    /// Add liquidity only: rejected if it would cross, else rests without matching.
    Alo,
    /// Immediate or cancel: match, drop the remainder.
    Ioc,
    /// Any other order type (trigger, `FrontendMarket`, unknown): the leg is
    /// answered [`OrderStatus::Unsupported`] and the book is untouched.
    Other,
}

/// One order leg of an `order` / `modify` / `batchModify` action, already
/// converted to ticks and lots by the driver (a string that does not convert
/// is the driver's to report; see the crate docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OrderReq {
    /// Asset index.
    pub asset: Asset,
    /// Buy (`b: true`) or sell.
    pub side: Side,
    /// Limit price in ticks.
    pub px: Px,
    /// Size in lots.
    pub sz: Sz,
    /// Reduce-only flag. Needs position state: such a leg is answered
    /// [`OrderStatus::Unsupported`] and the book is untouched.
    pub reduce_only: bool,
    /// Time in force.
    pub tif: Tif,
    /// Client order id, if any.
    pub cloid: Option<Cloid>,
    /// Order-priority fee rate of the action (`grouping: {"p": N}`), 0 for
    /// `"na"`. Used only for ALO queue placement (`ACTION_ORDERING` 3.3).
    pub priority: u64,
    /// A rejection the driver found from the action and static asset
    /// metadata only (never from native's response), e.g. `TickSize`. The
    /// leg gets this status, consumes no oid and does not touch the book.
    pub pre_reject: Option<RejectReason>,
    /// The oid native assigned to this leg, when the driver knows it from
    /// native's response. Used ONLY for comparison: the engine always uses
    /// its own counter and counts a differing hint in
    /// [`BlockOutcome::oid_mismatches`]. On a leg that takes no oid (a
    /// pre-reject, size or price 0, a modify of an unknown order or asset)
    /// any `Some` hint counts, since native spent an oid the engine did not.
    pub oid_hint: Option<Oid>,
}

/// Which order a cancel / modify names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderRef {
    /// By order id.
    Oid(Oid),
    /// By client order id (looked up per user).
    Cloid(Cloid),
}

/// One cancel leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CancelReq {
    /// Asset index the order rests on.
    pub asset: Asset,
    /// The order.
    pub target: OrderRef,
}

/// One modify leg: cancel `target`, place `order`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModifyReq {
    /// The order to replace.
    pub target: OrderRef,
    /// The replacement.
    pub order: OrderReq,
}

/// One signed user action of a block, borrowed from the driver's decode
/// buffers. Only order-book actions are modelled; everything else is
/// [`UserAction::Unsupported`] and makes its block not reproduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAction<'a> {
    /// `order`: legs in array order (trigger orders and TP/SL groupings are
    /// `Unsupported` at decode).
    Order {
        /// Signer.
        user: User,
        /// Legs.
        orders: &'a [OrderReq],
    },
    /// `cancel` / `cancelByCloid`.
    Cancel {
        /// Signer.
        user: User,
        /// Legs.
        cancels: &'a [CancelReq],
    },
    /// `modify` (one leg) or `batchModify`.
    Modify {
        /// Signer.
        user: User,
        /// Legs.
        modifies: &'a [ModifyReq],
    },
    /// `noop`: changes nothing in the modelled state.
    Noop,
    /// Any action the engine does not model (its `type` string).
    Unsupported(&'a str),
}

/// Why a leg was rejected by a rule the engine models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// Zero size.
    ZeroSize,
    /// Zero price.
    ZeroPrice,
    /// More than 5 significant figures: native's "Price must be divisible by tick size."
    TickSize,
    /// More decimals than the price grid allows: native's "Order has invalid price."
    InvalidPrice,
    /// More decimals than `szDecimals`: native's "Order has invalid size."
    LotSize,
    /// ALO that would cross: native's "Post only order would have
    /// immediately matched, bbo was BID@ASK". The best bid and ask at that moment.
    AloWouldCross {
        /// Best bid.
        bid: Option<Px>,
        /// Best ask.
        ask: Option<Px>,
    },
    /// IOC that found nothing to match.
    IocNoMatch,
    /// Cancel of an order that is not resting (or not this user's): native's
    /// "Order was never placed, already canceled, or filled."
    UnknownOrder,
    /// Modify of an order that is not resting: native's "Cannot modify
    /// canceled or filled order"; nothing is placed.
    ModifyUnknownOrder,
    /// Asset not in the engine's table.
    UnknownAsset,
    /// The order's oid (hint) is already resting.
    DuplicateOid,
    /// A level total would exceed `u64::MAX` lots.
    LevelOverflow,
    /// The book storage is full (preallocated capacity reached).
    CapacityExhausted,
}

/// Outcome of one leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    /// An order leg: what matched and what rests. Native shows `resting{oid}`
    /// when `rested > 0`, `filled{totalSz, avgPx, oid}` when it filled fully;
    /// `avg_px = notional / filled` (rounding to native's string is the
    /// driver's, UNVERIFIED).
    Placed {
        /// Oid used.
        oid: Oid,
        /// Total filled size in lots.
        filled: Sz,
        /// Sum of `px * sz` over the fills (ticks x lots).
        notional: u128,
        /// Size that came to rest (0 for IOC and fully filled orders).
        rested: Sz,
    },
    /// A cancel succeeded (native `"success"`).
    Cancelled,
    /// Rejected by a modelled rule; book unchanged by this leg.
    Rejected(RejectReason),
    /// Needs state or rules the engine does not model; book unchanged.
    Unsupported,
}

/// Per-action response: a range into [`BlockOutcome::statuses`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ActionResponse {
    /// First status index.
    pub start: u32,
    /// One past the last status index.
    pub end: u32,
    /// The whole action was unsupported (status range empty).
    pub unsupported: bool,
    /// Rejected whole as "Action already expired" (`expiresAfter` < block
    /// time); status range empty, no oid taken, nothing changed.
    pub expired: bool,
}

/// What a book diff did, mirroring hl-node `raw_book_diff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffKind {
    /// Came to rest with `sz`, at the back of its level or in front of `insert_before`.
    New {
        /// Resting size.
        sz: Sz,
        /// Oid it goes in front of.
        insert_before: Option<Oid>,
    },
    /// Size changed in place.
    Update {
        /// Size before.
        orig: Sz,
        /// Size after (non-zero).
        new: Sz,
    },
    /// Left the book.
    Remove,
}

/// One book diff, in application order (hl-node `node_raw_book_diffs` event).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BookDiff {
    /// Asset.
    pub asset: Asset,
    /// Owner.
    pub user: User,
    /// Order id.
    pub oid: Oid,
    /// Side.
    pub side: Side,
    /// Price.
    pub px: Px,
    /// The change.
    pub kind: DiffKind,
    /// Client order id of a `New` order. Native raw diffs do not carry it;
    /// a driver feeding [`crate::Engine::resync`] fills it from the block's
    /// order statuses so later `cancelByCloid` still resolves. Ignore it
    /// when comparing with native diffs.
    pub cloid: Option<Cloid>,
}

/// One fill against a resting maker, at the maker's price.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fill {
    /// Asset.
    pub asset: Asset,
    /// Taker oid.
    pub taker: Oid,
    /// Taker user.
    pub taker_user: User,
    /// Taker side.
    pub taker_side: Side,
    /// Maker oid.
    pub maker: Oid,
    /// Maker user.
    pub maker_user: User,
    /// Price (maker's).
    pub px: Px,
    /// Size.
    pub sz: Sz,
}

/// Everything one block produced. Buffers are reused across blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockOutcome {
    /// Block time (ms) as given.
    pub block_time_ms: u64,
    /// One response per input action, in input order.
    pub responses: Vec<ActionResponse>,
    /// Leg statuses, indexed by [`ActionResponse`] ranges.
    pub statuses: Vec<OrderStatus>,
    /// Book diffs in application order.
    pub diffs: Vec<BookDiff>,
    /// Fills in execution order.
    pub fills: Vec<Fill>,
    /// At least one action or leg was unsupported: the block cannot count as reproduced.
    pub has_unsupported: bool,
    /// Legs whose `oid_hint` differed from the engine's own counter, or
    /// was `Some` on a leg that took no oid.
    pub oid_mismatches: u32,
}

impl BlockOutcome {
    /// The leg statuses of action `i`.
    #[must_use]
    pub fn statuses_of(&self, i: usize) -> &[OrderStatus] {
        self.responses.get(i).map_or(&[], |r| {
            let s = usize::try_from(r.start).unwrap_or(usize::MAX);
            let e = usize::try_from(r.end).unwrap_or(usize::MAX);
            self.statuses.get(s..e).unwrap_or(&[])
        })
    }

    pub(crate) fn clear(&mut self, block_time_ms: u64) {
        self.block_time_ms = block_time_ms;
        self.responses.clear();
        self.statuses.clear();
        self.diffs.clear();
        self.fills.clear();
        self.has_unsupported = false;
        self.oid_mismatches = 0;
    }
}

/// One resting order, as loaded from an `l4Snapshots` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RestingOrder {
    /// Asset.
    pub asset: Asset,
    /// Owner.
    pub user: User,
    /// Order id.
    pub oid: Oid,
    /// Side.
    pub side: Side,
    /// Price.
    pub px: Px,
    /// Remaining size.
    pub sz: Sz,
    /// Client order id, if the snapshot carries one.
    pub cloid: Option<Cloid>,
    /// Placement time (ms), for ALO priority (0 when unknown: locked).
    pub t_ms: u64,
}

/// Preallocation sizes. Exceeding them still works but allocates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Capacity {
    /// Resting orders across all assets.
    pub orders: usize,
    /// Price levels per side per asset.
    pub levels_per_side: usize,
    /// Actions per block (response buffer).
    pub actions_per_block: usize,
    /// Legs, diffs and fills per block.
    pub events_per_block: usize,
}

impl Default for Capacity {
    fn default() -> Self {
        Self {
            orders: 1 << 20,
            levels_per_side: 1024,
            actions_per_block: 8192,
            events_per_block: 1 << 16,
        }
    }
}
