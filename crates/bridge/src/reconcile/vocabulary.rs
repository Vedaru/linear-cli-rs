//! The vocabulary a plan is written in: which side is which, and what an issue's
//! state means.
//!
//! Split out of `reconcile/mod.rs` (VED-288), which re-exports these so no path changes.

/// Which end of a mapping something happened on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Source,
    Sink,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Side::Source => Side::Sink,
            Side::Sink => Side::Source,
        }
    }
}

/// A value per side, so a policy cannot be quietly read from the wrong one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sides<T> {
    pub source: T,
    pub sink: T,
}

impl<T> Sides<T> {
    pub fn new(source: T, sink: T) -> Self {
        Self { source, sink }
    }

    pub fn get(&self, side: Side) -> &T {
        match side {
            Side::Source => &self.source,
            Side::Sink => &self.sink,
        }
    }

    pub fn of(&self, side: Side) -> &T {
        self.get(side)
    }
}

/// Which way a mapping mirrors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Both,
    /// Changes made on the source platform are mirrored to the sink; the sink is
    /// read but never written back from.
    SourceToSink,
    SinkToSource,
}

impl Direction {
    /// May a change observed on `side` be written to the other side?
    pub fn allows(&self, side: Side) -> bool {
        match self {
            Direction::Both => true,
            Direction::SourceToSink => side == Side::Source,
            Direction::SinkToSource => side == Side::Sink,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "both" | "two-way" | "twoway" => Some(Direction::Both),
            "source-to-sink" | "oneway" | "one-way" => Some(Direction::SourceToSink),
            "sink-to-source" => Some(Direction::SinkToSource),
            _ => None,
        }
    }
}

/// The platform's states, as the deployment names them.
///
/// Deployment-specific rather than platform-specific: which Linear state means
/// "finished" is a fact about a workspace, not about Linear. Names are matched
/// case-insensitively, because a human writing `Done` and the API returning `Done`
/// differ only in a way nobody means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateNames {
    /// Every name that means "finished". A list, because a workflow usually has
    /// more than one (`Done` and `Canceled`) and a mirror that knew only one of
    /// them would keep re-opening issues that were deliberately closed.
    pub closed: Vec<String>,
    /// Where a *newly created* mirrored issue lands. Linear's own default is
    /// `Backlog`; a mirrored issue that nobody planned lands in `Todo`.
    pub initial: Option<String>,
    /// The state to move an issue into when it is re-opened here.
    pub open: Option<String>,
}

impl StateNames {
    /// A closedness reading that is always defined: a state that is not configured
    /// as finished counts as open.
    ///
    /// Total on purpose. Leaving an unrecognised name undefined would make the two
    /// sides' content keys skip the state altogether, so a `Todo` issue and a
    /// `closed` one could look like the same revision - and the mirror would stop
    /// syncing state changes it could not name. Erring towards "open" is the safe
    /// direction: the worst case is an issue reopened that someone had closed with
    /// a status nobody configured.
    pub fn openness(&self, state: Option<&str>) -> Openness {
        match state {
            Some(state) if self.is_closed(state) => Openness::Closed,
            _ => Openness::Open,
        }
    }

    fn is_closed(&self, state: &str) -> bool {
        let state = state.trim();
        self.closed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(state))
    }

    /// The name to write to make this side have the given openness.
    pub fn name_for(&self, openness: Openness) -> Option<&str> {
        match openness {
            Openness::Closed => self.closed.first().map(String::as_str),
            // A reopen has to land somewhere specific; the initial state is the
            // honest fallback when the deployment named no open state.
            Openness::Open => self.open.as_deref().or(self.initial.as_deref()),
        }
    }
}

/// The part of a state both sides can agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Openness {
    Open,
    Closed,
}

impl Openness {
    pub(super) fn tag(self) -> &'static str {
        match self {
            Openness::Open => "open",
            Openness::Closed => "closed",
        }
    }
}
