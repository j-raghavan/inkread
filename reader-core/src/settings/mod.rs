//! Typed settings schema (RR23, ADR Decision 4).
//!
//! One model with **global** and **per-book** scope. Each [`SettingKey`] carries a type, a
//! built-in default, its scope class, and the schema version it was introduced in — all in the
//! [`registry`]. The core reads an immutable [`SettingsSnapshot`]; the shell writes through a
//! setter that bumps the version. Resolution is **per-book → global → built-in default**, and a
//! missing or type-mismatched value never panics — it falls back to the registered default
//! (RR23-FR3).
//!
//! Some keys (EPUB typesetting, pen) are **registered but inert** in M1a — stored so migrations
//! stay forward-compatible, consumed in M2/M1c.

use std::collections::HashMap;

use crate::persistence::BookId;

/// The scope a setting value applies to (RR23-FR1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Applies to every book unless a per-book value overrides it.
    Global,
    /// Applies to one book, overriding the global value.
    Book(BookId),
}

/// A typed setting value. `Enum` carries a small integer discriminant (e.g. a view mode).
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// A boolean toggle.
    Bool(bool),
    /// A signed integer (counts, percentages, enum discriminants).
    Int(i64),
    /// A free-text value (font family, storage roots).
    Text(String),
}

impl SettingValue {
    fn as_bool(&self) -> Option<bool> {
        match self {
            SettingValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn as_int(&self) -> Option<i64> {
        match self {
            SettingValue::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn as_text(&self) -> Option<&str> {
        match self {
            SettingValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The storage type tag (`0` bool, `1` int, `2` text) — persisted alongside the value.
    #[must_use]
    pub fn kind_code(&self) -> i64 {
        match self {
            SettingValue::Bool(_) => 0,
            SettingValue::Int(_) => 1,
            SettingValue::Text(_) => 2,
        }
    }

    /// Serialize the value to its storage string (paired with [`Self::kind_code`]).
    #[must_use]
    pub fn to_storage(&self) -> String {
        match self {
            SettingValue::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            SettingValue::Int(i) => i.to_string(),
            SettingValue::Text(s) => s.clone(),
        }
    }

    /// Reconstruct a value from a stored `(kind, value)` pair; `None` for an unknown kind or
    /// unparseable int (the caller defaults it — RR23-FR3, never crash).
    #[must_use]
    pub fn from_storage(kind: i64, value: &str) -> Option<SettingValue> {
        match kind {
            0 => Some(SettingValue::Bool(value == "1")),
            1 => value.parse::<i64>().ok().map(SettingValue::Int),
            2 => Some(SettingValue::Text(value.to_string())),
            _ => None,
        }
    }
}

/// The v1 setting catalog (RR23-FR2). Each maps to its feature (RR3/RR9/RR16/RR19/RR22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingKey {
    // Reading (RR25)
    DefaultViewMode, // 0 = paged, 1 = scroll
    TapZones,        // discriminant for the tap-zone map
    PageAnimation,   // 0 = none, 1 = slide
    // E-ink (RR3/RR16) — consumed by the policy in M1a
    FlashInterval,
    NightFlashInterval,
    AvoidFlashing,
    NightMode,
    DitherMode, // 0 = none, 1 = ordered, 2 = floyd-steinberg
    // Typesetting (RR9, per-book, EPUB) — registered but inert until M2
    FontSize,
    FontFamily,
    LineSpacing,
    Margin,
    Alignment,
    Hyphenation,
    // Pen (RR19) — registered but inert until M1c
    DefaultTool,
    PenColor,
    PenWidth,
    PalmRejection,
    // System (RR22)
    StorageRoots,
    LibrarySort,
}

impl SettingKey {
    /// Every key — the iteration source for snapshot building and the round-trip test.
    pub const ALL: [SettingKey; 20] = [
        SettingKey::DefaultViewMode,
        SettingKey::TapZones,
        SettingKey::PageAnimation,
        SettingKey::FlashInterval,
        SettingKey::NightFlashInterval,
        SettingKey::AvoidFlashing,
        SettingKey::NightMode,
        SettingKey::DitherMode,
        SettingKey::FontSize,
        SettingKey::FontFamily,
        SettingKey::LineSpacing,
        SettingKey::Margin,
        SettingKey::Alignment,
        SettingKey::Hyphenation,
        SettingKey::DefaultTool,
        SettingKey::PenColor,
        SettingKey::PenWidth,
        SettingKey::PalmRejection,
        SettingKey::StorageRoots,
        SettingKey::LibrarySort,
    ];

    /// The stable persisted name (the `settings.key` column). Exhaustive match — adding a key
    /// forces a name here.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKey::DefaultViewMode => "default_view_mode",
            SettingKey::TapZones => "tap_zones",
            SettingKey::PageAnimation => "page_animation",
            SettingKey::FlashInterval => "flash_interval",
            SettingKey::NightFlashInterval => "night_flash_interval",
            SettingKey::AvoidFlashing => "avoid_flashing",
            SettingKey::NightMode => "night_mode",
            SettingKey::DitherMode => "dither_mode",
            SettingKey::FontSize => "font_size",
            SettingKey::FontFamily => "font_family",
            SettingKey::LineSpacing => "line_spacing",
            SettingKey::Margin => "margin",
            SettingKey::Alignment => "alignment",
            SettingKey::Hyphenation => "hyphenation",
            SettingKey::DefaultTool => "default_tool",
            SettingKey::PenColor => "pen_color",
            SettingKey::PenWidth => "pen_width",
            SettingKey::PalmRejection => "palm_rejection",
            SettingKey::StorageRoots => "storage_roots",
            SettingKey::LibrarySort => "library_sort",
        }
    }

    /// Parse a persisted name back into a key; `None` for an unknown/old key (defaulted, not a
    /// crash — RR23-FR3).
    #[must_use]
    pub fn from_name(name: &str) -> Option<SettingKey> {
        SettingKey::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// Per-key metadata: built-in default, scope class, and the schema version it appeared in.
pub mod registry {
    use super::{SettingKey, SettingValue};

    /// Whether a key is global or per-book (RR23-FR1).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ScopeClass {
        /// One value for the whole app.
        Global,
        /// A value per book, falling back to the global/default.
        PerBook,
    }

    /// The registered metadata for a key.
    #[derive(Debug, Clone)]
    pub struct KeyMeta {
        /// The built-in default value (and implicitly the key's type).
        pub default: SettingValue,
        /// Whether the key is global or per-book.
        pub scope: ScopeClass,
        /// The schema version that introduced the key (RR23-FR3 migration).
        pub since: u32,
    }

    /// Look up a key's metadata (the single source of truth for type/default/scope).
    #[must_use]
    pub fn meta(key: SettingKey) -> KeyMeta {
        use ScopeClass::{Global, PerBook};
        use SettingKey as K;
        use SettingValue::{Bool, Int, Text};
        let (default, scope) = match key {
            K::DefaultViewMode => (Int(0), Global),
            K::TapZones => (Int(0), Global),
            K::PageAnimation => (Int(0), Global),
            K::FlashInterval => (Int(6), Global),
            K::NightFlashInterval => (Int(6), Global),
            K::AvoidFlashing => (Bool(false), Global),
            K::NightMode => (Bool(false), Global),
            K::DitherMode => (Int(1), Global), // ordered, the e-ink default
            K::FontSize => (Int(100), PerBook),
            K::FontFamily => (Text(String::new()), PerBook),
            K::LineSpacing => (Int(100), PerBook),
            K::Margin => (Int(100), PerBook),
            K::Alignment => (Int(0), PerBook),
            K::Hyphenation => (Bool(true), PerBook),
            K::DefaultTool => (Int(0), Global),
            K::PenColor => (Int(0), Global),
            K::PenWidth => (Int(3), Global),
            K::PalmRejection => (Bool(true), Global),
            K::StorageRoots => (Text(String::new()), Global),
            K::LibrarySort => (Int(0), Global),
        };
        KeyMeta {
            default,
            scope,
            since: 1,
        }
    }
}

/// The immutable settings snapshot the core reads (RR23-FR1).
///
/// Built from the persisted values + a `version`; resolution falls back per-book → global →
/// built-in default. Cloned, never mutated in place — the shell builds a fresh snapshot (with a
/// bumped version) when a setting changes.
#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    version: u32,
    global: HashMap<SettingKey, SettingValue>,
    per_book: HashMap<(BookId, SettingKey), SettingValue>,
}

impl SettingsSnapshot {
    /// A snapshot at `version` from the given scoped values (the rest fall back to defaults).
    pub fn from_values(
        version: u32,
        values: impl IntoIterator<Item = (Scope, SettingKey, SettingValue)>,
    ) -> Self {
        let mut global = HashMap::new();
        let mut per_book = HashMap::new();
        for (scope, key, value) in values {
            match scope {
                Scope::Global => {
                    global.insert(key, value);
                }
                Scope::Book(book) => {
                    per_book.insert((book, key), value);
                }
            }
        }
        Self {
            version,
            global,
            per_book,
        }
    }

    /// An all-defaults snapshot at version `version`.
    #[must_use]
    pub fn defaults(version: u32) -> Self {
        Self::from_values(version, std::iter::empty())
    }

    /// The schema/config version (bumped by the shell on each write).
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Resolve a key for an optional book: per-book → global → built-in default (RR23-FR3).
    fn resolve(&self, key: SettingKey, book: Option<&BookId>) -> SettingValue {
        if let Some(b) = book {
            if let Some(v) = self.per_book.get(&(b.clone(), key)) {
                return v.clone();
            }
        }
        if let Some(v) = self.global.get(&key) {
            return v.clone();
        }
        registry::meta(key).default
    }

    /// Typed bool getter; a missing/mismatched value yields the registered default (else false).
    #[must_use]
    pub fn get_bool(&self, key: SettingKey, book: Option<&BookId>) -> bool {
        self.resolve(key, book)
            .as_bool()
            .or_else(|| registry::meta(key).default.as_bool())
            .unwrap_or(false)
    }

    /// Typed int getter; a missing/mismatched value yields the registered default (else 0).
    #[must_use]
    pub fn get_int(&self, key: SettingKey, book: Option<&BookId>) -> i64 {
        self.resolve(key, book)
            .as_int()
            .or_else(|| registry::meta(key).default.as_int())
            .unwrap_or(0)
    }

    /// Typed text getter; a missing/mismatched value yields the registered default (else "").
    #[must_use]
    pub fn get_text(&self, key: SettingKey, book: Option<&BookId>) -> String {
        match self.resolve(key, book) {
            SettingValue::Text(s) => s,
            _ => registry::meta(key)
                .default
                .as_text()
                .unwrap_or_default()
                .to_string(),
        }
    }

    // ---- Policy views (RR3 ↔ RR23): the refresh policy is built from these. ----

    /// The day flash-promotion interval (RR3-FR3 / RR16), clamped to ≥ 1.
    #[must_use]
    pub fn flash_interval(&self, book: Option<&BookId>) -> u32 {
        self.get_int(SettingKey::FlashInterval, book)
            .clamp(1, i64::from(u32::MAX)) as u32
    }

    /// The night flash-promotion interval (RR3-FR6), clamped to ≥ 1.
    #[must_use]
    pub fn night_flash_interval(&self, book: Option<&BookId>) -> u32 {
        self.get_int(SettingKey::NightFlashInterval, book)
            .clamp(1, i64::from(u32::MAX)) as u32
    }

    /// Whether to trade ghosting for a flash-free experience (RR3-FR7).
    #[must_use]
    pub fn avoid_flashing(&self, book: Option<&BookId>) -> bool {
        self.get_bool(SettingKey::AvoidFlashing, book)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> BookId {
        BookId::new("b1").unwrap()
    }

    #[test]
    fn missing_key_returns_registered_default() {
        let s = SettingsSnapshot::defaults(1);
        assert_eq!(s.get_int(SettingKey::FlashInterval, None), 6);
        assert_eq!(s.get_int(SettingKey::DitherMode, None), 1);
        assert!(!s.get_bool(SettingKey::AvoidFlashing, None));
        assert!(s.get_bool(SettingKey::Hyphenation, None)); // default true
        assert_eq!(s.get_text(SettingKey::FontFamily, None), "");
    }

    #[test]
    fn per_book_overrides_global_overrides_default() {
        let b = book();
        let s = SettingsSnapshot::from_values(
            2,
            [
                (
                    Scope::Global,
                    SettingKey::FlashInterval,
                    SettingValue::Int(8),
                ),
                (
                    Scope::Book(b.clone()),
                    SettingKey::FlashInterval,
                    SettingValue::Int(3),
                ),
            ],
        );
        // No book → global (8). With the book → per-book (3). A different book → global (8).
        assert_eq!(s.get_int(SettingKey::FlashInterval, None), 8);
        assert_eq!(s.get_int(SettingKey::FlashInterval, Some(&b)), 3);
        let other = BookId::new("other").unwrap();
        assert_eq!(s.get_int(SettingKey::FlashInterval, Some(&other)), 8);
        // An unset key still resolves to its built-in default.
        assert_eq!(s.get_int(SettingKey::NightFlashInterval, Some(&b)), 6);
        assert_eq!(s.version(), 2);
    }

    #[test]
    fn type_mismatch_falls_back_to_default_not_panic() {
        // Store an Int under a Bool key (a malformed/legacy value) — the bool getter must not
        // panic; it falls back to the registered default (false).
        let s = SettingsSnapshot::from_values(
            1,
            [(
                Scope::Global,
                SettingKey::AvoidFlashing,
                SettingValue::Int(7),
            )],
        );
        assert!(!s.get_bool(SettingKey::AvoidFlashing, None));
    }

    #[test]
    fn setting_key_names_round_trip_and_are_unique() {
        use std::collections::HashSet;
        let mut names = HashSet::new();
        for k in SettingKey::ALL {
            assert_eq!(SettingKey::from_name(k.as_str()), Some(k));
            assert!(names.insert(k.as_str()), "duplicate name {}", k.as_str());
        }
        assert!(SettingKey::from_name("nope_unknown").is_none());
    }

    #[test]
    fn setting_value_storage_round_trips() {
        for v in [
            SettingValue::Bool(true),
            SettingValue::Bool(false),
            SettingValue::Int(-7),
            SettingValue::Text("hi".into()),
        ] {
            let round = SettingValue::from_storage(v.kind_code(), &v.to_storage()).unwrap();
            assert_eq!(round, v);
        }
        // Unknown kind / unparseable int → None (defaulted by the caller).
        assert!(SettingValue::from_storage(9, "x").is_none());
        assert!(SettingValue::from_storage(1, "notanint").is_none());
    }

    #[test]
    fn policy_view_getters_resolve_and_clamp() {
        let s = SettingsSnapshot::from_values(
            1,
            [
                (
                    Scope::Global,
                    SettingKey::FlashInterval,
                    SettingValue::Int(4),
                ),
                (
                    Scope::Global,
                    SettingKey::NightFlashInterval,
                    SettingValue::Int(2),
                ),
                (
                    Scope::Global,
                    SettingKey::AvoidFlashing,
                    SettingValue::Bool(true),
                ),
            ],
        );
        assert_eq!(s.flash_interval(None), 4);
        assert_eq!(s.night_flash_interval(None), 2);
        assert!(s.avoid_flashing(None));

        // Defaults + clamp: a 0 interval clamps to 1; an unset interval is the default 6.
        let z = SettingsSnapshot::from_values(
            1,
            [(
                Scope::Global,
                SettingKey::FlashInterval,
                SettingValue::Int(0),
            )],
        );
        assert_eq!(z.flash_interval(None), 1);
        assert_eq!(SettingsSnapshot::defaults(1).flash_interval(None), 6);
    }

    #[test]
    fn registry_scope_classes_are_as_specified() {
        use registry::ScopeClass;
        assert_eq!(
            registry::meta(SettingKey::FontSize).scope,
            ScopeClass::PerBook
        );
        assert_eq!(
            registry::meta(SettingKey::FlashInterval).scope,
            ScopeClass::Global
        );
    }
}
