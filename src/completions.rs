//! Completion lists an embedder can supply at construction time.
//!
//! By default the server falls back to the lists embedded from `data/*.txt`.
//! An embedder that knows better — a CLI holding the user's own pantry, a
//! localized deployment, a test — replaces any of them through the builder
//! methods on [`Backend`](crate::Backend).
//!
//! Replacement is per list and reaches only that fallback tier: suggestions
//! drawn from the open documents and from `aisle.conf` are never affected.

/// An ingredient or cookware suggestion.
///
/// Only `name` is required. Construct with [`ItemEntry::new`], from a string,
/// or with a struct literal and `..Default::default()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemEntry {
    /// Completion label, and the text inserted before the `{}` placeholder.
    pub name: String,
    /// Shown beside the label. Falls back to `category`, then to a generic
    /// description such as "Common ingredient".
    pub detail: Option<String>,
    /// Grouping such as an aisle or shelf. Used as the detail when `detail`
    /// is absent, mirroring how `aisle.conf` entries are presented.
    pub category: Option<String>,
}

impl ItemEntry {
    /// Creates an entry carrying only a name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: None,
            category: None,
        }
    }
}

impl From<String> for ItemEntry {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

impl From<&str> for ItemEntry {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// A measurement or time unit suggestion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitEntry {
    /// Completion label and inserted text, e.g. `tbsp`.
    pub symbol: String,
    /// Expanded form shown beside the symbol, e.g. `tablespoons`.
    pub name: Option<String>,
}

impl UnitEntry {
    /// Creates a unit with both its symbol and expanded name.
    pub fn new(symbol: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            name: Some(name.into()),
        }
    }

    /// Creates a unit with no expanded name.
    pub fn symbol_only(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            name: None,
        }
    }
}

impl From<String> for UnitEntry {
    fn from(symbol: String) -> Self {
        Self::symbol_only(symbol)
    }
}

impl From<&str> for UnitEntry {
    fn from(symbol: &str) -> Self {
        Self::symbol_only(symbol)
    }
}

/// The injected lists, one `Option` per fallback list.
///
/// `None` keeps the built-in list; `Some` replaces it, and `Some(vec![])`
/// suppresses that list's fallback suggestions entirely.
#[derive(Debug, Default)]
pub(crate) struct CustomCompletions {
    pub(crate) ingredients: Option<Vec<ItemEntry>>,
    pub(crate) cookware: Option<Vec<ItemEntry>>,
    pub(crate) units: Option<Vec<UnitEntry>>,
    pub(crate) time_units: Option<Vec<UnitEntry>>,
}
