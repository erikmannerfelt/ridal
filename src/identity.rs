//! Persistent identity metadata for processed radargrams.
//!
//! Three concepts that the web catalog (#115) and gprinterp need kept apart:
//!
//! - [`RadargramId`]: the stable, user-facing identity of one conceptual
//!   radargram. Independent of filename, path, display name and processing
//!   settings. Referenced by gprinterp for interpretation matching.
//! - [`GroupId`]: an optional grouping label (survey, campaign, location)
//!   used only by the catalog and index UI. No identity semantics.
//! - [`DisplayName`]: an optional human-facing label. No identity semantics;
//!   must never affect revision or render identity.
//!
//! - [`UserId`]: who authored an interpretation. Ridal has no authentication
//!   yet, so today this is always the literal `default`; it exists as a
//!   validated type from the start because it is used as a *filename* inside
//!   a project (`interpretations/<radargram>/<user>.gprinterp.json`), and a
//!   name that arrives over HTTP must never be able to escape that
//!   directory.
//!
//! `RadargramId` and `GroupId` share validation rules because both are used
//! in path-like ways by the web server (#116): ASCII lowercase,
//! `[a-z0-9_-]`, 1-128 characters, no leading/trailing separator, and not a
//! reserved name.

use std::fmt;

const MAX_SLUG_LEN: usize = 128;

/// Names disallowed for any slug because both radargram and group IDs are
/// used in URL-path and (potentially) filesystem-adjacent contexts.
const RESERVED_SLUGS: &[&str] = &[
    ".", "..", "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn is_valid_slug_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

/// Validate `value` as a slug of the given `kind` (used only in error text).
///
/// Rejects with a specific, actionable error rather than silently sanitizing;
/// callers that want sanitized fallback behavior use [`sanitize_to_slug`]
/// instead and validate its result.
fn validate_slug(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{kind} must not be empty"));
    }
    if value.chars().count() > MAX_SLUG_LEN {
        return Err(format!(
            "{kind} '{value}' is too long ({} chars, max {MAX_SLUG_LEN})",
            value.chars().count()
        ));
    }
    if let Some(bad) = value.chars().find(|c| !is_valid_slug_char(*c)) {
        return Err(format!(
            "{kind} '{value}' contains disallowed character '{bad}'. \
             Only lowercase ASCII letters, digits, '-' and '_' are allowed."
        ));
    }
    if value.starts_with(['-', '_']) || value.ends_with(['-', '_']) {
        return Err(format!(
            "{kind} '{value}' must not start or end with '-' or '_'"
        ));
    }
    if RESERVED_SLUGS.contains(&value) {
        return Err(format!("{kind} '{value}' is a reserved name"));
    }
    Ok(())
}

/// Transliterate the Nordic letters into their conventional ASCII forms.
///
/// Without this, the charset filter in [`sanitize_to_slug`] treats them as
/// unsupported and collapses each to `-`, so "Drønbreen" would become
/// "dr-nbreen" and "Ålesund" would become "lesund" (the leading separator
/// is trimmed). That is a poor default for a tool whose domain is Svalbard
/// and mainland Norwegian glaciology, where these letters are common in
/// place names.
///
/// Deliberately narrow: only ø/æ/å, the three letters of the Norwegian
/// alphabet beyond ASCII. Broader Latin-1 folding (ä, ö, é, ñ, ...) would
/// need either a much longer table or a dependency, and neither is
/// justified by the data this tool actually sees. #116 permits
/// slugification as long as the output satisfies the ASCII rules.
fn transliterate_nordic(c: char) -> Option<&'static str> {
    match c {
        'ø' | 'Ø' => Some("o"),
        'æ' | 'Æ' => Some("ae"),
        'å' | 'Å' => Some("aa"),
        _ => None,
    }
}

/// Lowercase `stem`, transliterate Nordic letters, collapse runs of
/// unsupported characters to `-`, and trim leading/trailing separators.
/// Deterministic: the same stem always produces the same slug. Does not
/// itself validate the result -- an all-separator or empty stem produces an
/// empty string, which the caller must reject with an actionable error
/// rather than accept silently.
fn sanitize_to_slug(stem: &str) -> String {
    let lowered = stem.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_sep = false;
    for c in lowered.chars() {
        // Transliteration runs before the charset check, so its output
        // ("o", "ae", "aa") is always already valid and never treated as a
        // separator.
        if let Some(ascii) = transliterate_nordic(c) {
            out.push_str(ascii);
            last_was_sep = false;
        } else if is_valid_slug_char(c) {
            out.push(c);
            last_was_sep = c == '-' || c == '_';
        } else if !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }
    out.trim_matches(['-', '_']).to_string()
}

/// Transliterate the letters that have an obvious ASCII mapping for use in an
/// *expression identifier* (#206).
///
/// Deliberately different from [`transliterate_nordic`], which is the slug
/// rule: a slug may contain both `-` and `_` and maps `å` to `aa`, while an
/// identifier must match `^[a-z][a-z0-9_]*$`, so `å` maps to `a` and `æ` to
/// `ae`. The two tables cannot be shared without one of them becoming wrong.
///
/// A letter with no mapping here is dropped rather than turned into a
/// separator, so `Drønbreen` is `dronbreen` and not `dr_nbreen`.
fn transliterate_identifier(c: char) -> Option<&'static str> {
    match c {
        'ø' | 'Ø' => Some("o"),
        'å' | 'Å' => Some("a"),
        'ä' | 'Ä' => Some("a"),
        'ö' | 'Ö' => Some("o"),
        'æ' | 'Æ' => Some("ae"),
        'é' | 'É' => Some("e"),
        _ => None,
    }
}

/// Is `s` usable as a variable name in a derived-layer expression (#206)?
///
/// The rule is `^[a-z][a-z0-9_]*$`. It is intentionally narrower than
/// [`validate_slug`]: a slug may contain `-` and start with `_`, neither of
/// which Rhai accepts in a bare identifier.
pub fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Rhai keywords and reserved words that cannot be a bare variable name.
const RHAI_KEYWORDS: &[&str] = &[
    "if",
    "else",
    "switch",
    "do",
    "while",
    "loop",
    "for",
    "in",
    "continue",
    "break",
    "return",
    "let",
    "const",
    "fn",
    "private",
    "import",
    "export",
    "as",
    "true",
    "false",
    "this",
    "global",
    "Fn",
    "call",
    "curry",
    "type_of",
    "print",
    "debug",
    "eval",
    "is_def_var",
    "is_def_fn",
    "is_shared",
];

/// Names of the expression built-ins. A layer called `count` would shadow the
/// `count` reduction, so it is suffixed rather than accepted as a variable.
const EXPRESSION_BUILTINS: &[&str] = &[
    "count",
    "median",
    "mean",
    "std",
    "nmad",
    "percentile",
    "percentile_lower",
    "min",
    "max",
    "concatenate",
    "shallowest",
    "deepest",
    "clamp",
    "where",
    "nan",
];

/// Turn a human display name into a valid, non-colliding Rhai identifier
/// (#206).
///
/// Rules, in order: lowercase; transliterate the letters in
/// [`transliterate_identifier`]; collapse every run of non-alphanumeric ASCII
/// to a single `_`; strip leading/trailing `_`; prefix `l_` when the result is
/// empty or starts with a digit; append `_layer` when the result is a Rhai
/// keyword or an expression built-in; append `_2`, `_3`, ... until the result
/// is not in `taken`.
///
/// `taken` is the set of ids already in use. The result always satisfies
/// [`is_valid_identifier`].
pub fn sanitize_to_identifier(display_name: &str, taken: &[String]) -> String {
    let lowered = display_name.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_sep = false;
    for c in lowered.chars() {
        if let Some(ascii) = transliterate_identifier(c) {
            out.push_str(ascii);
            last_was_sep = false;
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if c.is_ascii() {
            // A run of punctuation/whitespace collapses to one `_`. A leading
            // run is dropped by the `out.is_empty()` guard; a trailing run is
            // trimmed below.
            if !last_was_sep && !out.is_empty() {
                out.push('_');
                last_was_sep = true;
            }
        }
        // A non-ASCII character with no mapping is dropped outright, not
        // treated as a separator: `Drønbreen` must not become `dr_nbreen`.
    }
    while out.ends_with('_') {
        out.pop();
    }

    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "l_");
    }

    if RHAI_KEYWORDS.contains(&out.as_str()) || EXPRESSION_BUILTINS.contains(&out.as_str()) {
        out.push_str("_layer");
    }

    if taken.iter().any(|t| t == &out) {
        let base = out.clone();
        let mut n = 2;
        loop {
            let candidate = format!("{base}_{n}");
            if !taken.iter().any(|t| t == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }
    out
}

macro_rules! slug_newtype {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("A validated ", $kind, " slug.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validate `value` as an explicit user-supplied identifier.
            /// Rejects invalid input rather than sanitizing it.
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                validate_slug($kind, &value)?;
                Ok(Self(value))
            }

            /// Derive a slug from a fallback source (typically an output file
            /// stem), sanitizing deterministically. Fails with an actionable
            /// error if no valid slug remains after sanitation.
            ///
            /// `GroupId`'s fallback source is the catalog-relative parent
            /// directory, computed by catalog discovery (M3) rather than at
            /// export time, so this is unused until then.
            #[allow(dead_code)]
            pub fn from_fallback(stem: &str) -> Result<Self, String> {
                let sanitized = sanitize_to_slug(stem);
                if sanitized.is_empty() {
                    return Err(format!(
                        "Could not derive a valid {} from '{stem}': no valid \
                         characters remained after sanitization. Supply one \
                         explicitly.",
                        $kind
                    ));
                }
                // Sanitization guarantees charset and separator rules; only
                // length and reserved-name checks can still fail here.
                validate_slug($kind, &sanitized)?;
                Ok(Self(sanitized))
            }

            // Used by the catalog and inspector added in M2/M3; unused for
            // now since export.rs reaches the inner string via Display.
            #[allow(dead_code)]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        /// Deserializing runs the same validation `new` does, rather than
        /// trusting the file. These slugs are used as path components, so a
        /// hand-edited document must not be able to reintroduce a value the
        /// HTTP boundary would have rejected.
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

slug_newtype!(RadargramId, "radargram ID");
slug_newtype!(GroupId, "group");
slug_newtype!(UserId, "user");

/// The author recorded when Ridal has no authentication to ask.
///
/// Defined here rather than in the interpretation store or the level 2
/// exporter because both need it and they must agree: the filename a pick is
/// saved under and the `user` column it exports as are the same identity.
pub const DEFAULT_USER: &str = "default";

/// JSON round-tripping for the two free-form label types.
///
/// Deserializing goes through `from_input` for the same reason the slug
/// newtypes validate on the way in: a stored document must not be able to
/// hold a value the constructor would have refused. Here that means an
/// empty or whitespace-only label, which both types define as *absent* --
/// so it is rejected rather than accepted as a label that renders as
/// nothing.
macro_rules! serialize_as_string {
    ($name:ident, $kind:expr) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::from_input(&value)
                    .ok_or_else(|| serde::de::Error::custom(format!("{} must not be empty", $kind)))
            }
        }
    };
}

/// An optional human-facing label with no identity semantics. An empty or
/// whitespace-only value is treated as absent by [`DisplayName::from_input`]
/// rather than as a valid (empty) display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayName(String);

impl DisplayName {
    /// Returns `None` for empty or whitespace-only input, matching the rule
    /// that an absent display name should not be written to the output at
    /// all (#116).
    pub fn from_input(value: impl AsRef<str>) -> Option<Self> {
        let trimmed = value.as_ref().trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_string()))
        }
    }

    // Used by the catalog and viewer added in M3/M7; unused for now since
    // export.rs reaches the inner string via Display.
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

serialize_as_string!(DisplayName, "display name");

/// Resolve the effective radargram ID for a new or reprocessed output,
/// following the precedence from #116:
/// explicit `--radargram-id` > inherited `ridal_radargram_id` > output stem.
///
/// Returns the resolved ID together with a human-readable note when the
/// value came from the output-stem fallback, so the caller can print the
/// recommended informational warning.
pub fn resolve_radargram_id(
    explicit: Option<&str>,
    inherited: Option<&str>,
    output_stem: &str,
) -> Result<(RadargramId, Option<String>), String> {
    if let Some(explicit) = explicit {
        return Ok((RadargramId::new(explicit)?, None));
    }
    if let Some(inherited) = inherited {
        // Inherited values were valid when written; re-validate defensively
        // in case the source file predates this scheme or was hand-edited.
        return Ok((RadargramId::new(inherited)?, None));
    }
    let id = RadargramId::from_fallback(output_stem)?;
    let note = format!(
        "No radargram ID was supplied. Using output stem \"{id}\" as the \
         radargram ID.\n\nExplicitly assigning a stable, unique ID with \
         --radargram-id is recommended, particularly when processing \
         collections of radargrams."
    );
    Ok((id, Some(note)))
}

/// Resolve the effective display name, following the precedence from #116:
/// explicit `--display-name` > inherited `ridal_display_name` > absent.
///
/// An explicit flag that was *passed* but empty (`Some("")`) is a deliberate
/// clear, not a fall-through to `inherited`: `--display-name ""` is the only
/// way to remove an inherited display name on reprocessing. Only a wholly
/// absent explicit value (`None`, i.e. the flag was not given) falls
/// through.
pub fn resolve_display_name(
    explicit: Option<&str>,
    inherited: Option<&str>,
) -> Option<DisplayName> {
    match explicit {
        Some(value) => DisplayName::from_input(value),
        None => inherited.and_then(DisplayName::from_input),
    }
}

/// An optional human-facing group label with no identity semantics, no
/// character restrictions (Unicode is encouraged -- a group is a survey,
/// campaign, or place name, and those are not ASCII in general). Mirrors
/// [`DisplayName`] exactly: an empty or whitespace-only value is treated
/// as absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupName(String);

impl GroupName {
    /// Returns `None` for empty or whitespace-only input, matching
    /// [`DisplayName::from_input`]'s rule.
    pub fn from_input(value: impl AsRef<str>) -> Option<Self> {
        let trimmed = value.as_ref().trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(Self(trimmed.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GroupName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

serialize_as_string!(GroupName, "group name");

/// Resolve the effective group name, following the same precedence shape
/// as display name: explicit > inherited > absent. An explicit empty
/// override (`Some("")`) is a deliberate clear, matching
/// [`resolve_display_name`].
pub fn resolve_group_name(explicit: Option<&str>, inherited: Option<&str>) -> Option<GroupName> {
    match explicit {
        Some(value) => GroupName::from_input(value),
        None => inherited.and_then(GroupName::from_input),
    }
}

/// Resolve the effective group as a `(name, id)` pair, mirroring #116's
/// radargram-ID/display-name split one level up: a group's *name* is
/// free-form Unicode text a human enters, while its *id* is always a
/// validated [`GroupId`] slug for path-like uses (the catalog's
/// `data-group` key, `/api/v1/groups/{id}/tracks`). The id is derived
/// from the name via [`sanitize_to_slug`] (which already transliterates
/// ø/æ/å, so a name of "Drønbreen" derives id "dronbreen") unless
/// `explicit_id`/`inherited_id` override that derivation.
///
/// No name means no group at all: an id is never meaningful on its own,
/// since it exists only to give the name a URL/filesystem-safe form.
pub fn resolve_group(
    explicit_name: Option<&str>,
    explicit_id: Option<&str>,
    inherited_name: Option<&str>,
    inherited_id: Option<&str>,
) -> Result<Option<(GroupName, GroupId)>, String> {
    let Some(name) = resolve_group_name(explicit_name, inherited_name) else {
        return Ok(None);
    };
    let id = if let Some(id) = explicit_id {
        GroupId::new(id)?
    } else if let Some(id) = inherited_id {
        GroupId::new(id)?
    } else {
        GroupId::from_fallback(name.as_str())?
    };
    Ok(Some((name, id)))
}

/// A derived identity for one processed revision (#117). Changes when
/// reprocessing produces new output.
///
/// Lives here rather than in `server::catalog` because it has two consumers
/// with different feature requirements: the server invalidates cached
/// renders with it, and `ridal interp` records it as the provenance of a
/// level 2 export. Those two *must* produce the same string for the same
/// file -- an interpretation written by the GUI and one written by the CLI
/// naming different revisions for one radargram would be worse than naming
/// none -- so it cannot sit behind the `server` feature, and `blake3` is
/// consequently an unconditional dependency.
///
/// Still deliberately *not* computed by `io::inspect_ridal_netcdf` (#123,
/// M2), which stays a pure metadata recogniser.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RevisionId(String);

impl RevisionId {
    /// `FastRevisionFingerprintV1`: a declared processing-revision
    /// identifier, not a content-integrity checksum. Deliberately excludes
    /// path, filesystem timestamps, filesize and display name -- see #117
    /// for the full list of what must *not* change the revision.
    pub fn fingerprint_v1(radargram_id: &RadargramId, processing_datetime: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ridal-revision-v1");
        hasher.update(radargram_id.as_str().as_bytes());
        hasher.update(processing_datetime.as_bytes());
        // First 16 bytes (32 hex chars): a revision identifier needs to be
        // collision-resistant among one user's radargrams, not
        // cryptographically unforgeable, and the full 32-byte hex digest
        // would make already-long chunk/overview URLs harder to read.
        Self(hasher.finalize().to_hex()[..32].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_explicit_id_accepted() {
        assert!(RadargramId::new("kroppbreen-centerline-01").is_ok());
        assert!(RadargramId::new("a").is_ok());
        assert!(RadargramId::new("a_b-c9").is_ok());
    }

    #[test]
    fn uppercase_explicit_id_rejected() {
        let err = RadargramId::new("Kroppbreen").unwrap_err();
        assert!(err.contains("disallowed character"), "{err}");
    }

    #[test]
    fn leading_trailing_separator_rejected() {
        assert!(RadargramId::new("-leading").is_err());
        assert!(RadargramId::new("trailing-").is_err());
        assert!(RadargramId::new("_leading").is_err());
        assert!(RadargramId::new("trailing_").is_err());
    }

    #[test]
    fn reserved_names_rejected() {
        for reserved in [".", "..", "con", "com1", "nul"] {
            assert!(
                RadargramId::new(reserved).is_err(),
                "{reserved} should be rejected"
            );
        }
    }

    #[test]
    fn empty_id_rejected() {
        assert!(RadargramId::new("").is_err());
    }

    #[test]
    fn too_long_id_rejected() {
        let long = "a".repeat(129);
        assert!(RadargramId::new(&long).is_err());
        let ok = "a".repeat(128);
        assert!(RadargramId::new(&ok).is_ok());
    }

    #[test]
    fn fallback_sanitation_is_deterministic() {
        let a = RadargramId::from_fallback("Dronbreen 2022-03-29 (A1)").unwrap();
        let b = RadargramId::from_fallback("Dronbreen 2022-03-29 (A1)").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "dronbreen-2022-03-29-a1");
    }

    #[test]
    fn fallback_sanitation_collapses_unsupported_character_runs() {
        // #116's algorithm collapses runs of *unsupported* characters to a
        // single '-'. Characters that are already valid separators ('-'/'_')
        // pass through unchanged rather than being collapsed together.
        let id = RadargramId::from_fallback("a   b!!!c").unwrap();
        assert_eq!(id.as_str(), "a-b-c");
    }

    #[test]
    fn fallback_sanitation_preserves_existing_separator_runs() {
        let id = RadargramId::from_fallback("a___b---c").unwrap();
        assert_eq!(id.as_str(), "a___b---c");
    }

    #[test]
    fn nordic_letters_transliterate_rather_than_becoming_separators() {
        // Before transliteration existed these produced "dr-nbreen",
        // "kvit-ya" and "lesund" (the leading '-' being trimmed away),
        // which is a poor auto-derived ID for a Svalbard/Norwegian tool.
        assert_eq!(
            RadargramId::from_fallback("Drønbreen").unwrap().as_str(),
            "dronbreen"
        );
        assert_eq!(
            RadargramId::from_fallback("Kvitøya").unwrap().as_str(),
            "kvitoya"
        );
        assert_eq!(
            RadargramId::from_fallback("Ålesund").unwrap().as_str(),
            "aalesund"
        );
        assert_eq!(
            RadargramId::from_fallback("Blåbærdalen").unwrap().as_str(),
            "blaabaerdalen"
        );
    }

    #[test]
    fn nordic_and_ascii_spellings_produce_the_same_slug() {
        // The property that makes this change safe for existing catalogs:
        // a user who renames "Dronbreen_2022.nc" to "Drønbreen_2022.nc"
        // still gets the same radargram ID, so interpretations keyed by it
        // continue to match.
        assert_eq!(
            RadargramId::from_fallback("Drønbreen_2022").unwrap(),
            RadargramId::from_fallback("Dronbreen_2022").unwrap()
        );
    }

    #[test]
    fn transliterated_slugs_are_deterministic_and_valid() {
        let a = RadargramId::from_fallback("Drønbreen 2022-03-29 (A1)").unwrap();
        let b = RadargramId::from_fallback("Drønbreen 2022-03-29 (A1)").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "dronbreen-2022-03-29-a1");
        // Round-trips through the strict explicit-value validator, i.e. the
        // sanitizer cannot emit something `RadargramId::new` would reject.
        assert!(RadargramId::new(a.as_str()).is_ok());
    }

    #[test]
    fn uppercase_nordic_letters_also_transliterate() {
        // to_lowercase() runs first so these arrive lowercased, but the
        // mapping covers both cases explicitly rather than relying on that.
        assert_eq!(transliterate_nordic('Ø'), Some("o"));
        assert_eq!(transliterate_nordic('Æ'), Some("ae"));
        assert_eq!(transliterate_nordic('Å'), Some("aa"));
        assert_eq!(transliterate_nordic('a'), None);
    }

    #[test]
    fn explicit_ids_still_reject_nordic_letters() {
        // Transliteration is a *fallback* convenience for deriving an ID
        // from a filename. An explicitly supplied --radargram-id is still
        // validated strictly and rejected, rather than silently rewritten
        // into something the user did not type (#116: "with a clear error
        // rather than silently sanitizing").
        let err = RadargramId::new("drønbreen").unwrap_err();
        assert!(err.contains("disallowed character"), "{err}");
    }

    #[test]
    fn fallback_sanitation_trims_edges() {
        let id = RadargramId::from_fallback("--Weird Name!!--").unwrap();
        assert_eq!(id.as_str(), "weird-name");
    }

    #[test]
    fn fallback_sanitation_actionable_error_when_empty() {
        let err = RadargramId::from_fallback("!!!___---").unwrap_err();
        assert!(err.contains("Supply one explicitly"), "{err}");
    }

    #[test]
    fn fallback_sanitation_rejects_reserved_result() {
        // "CON" sanitizes to the non-empty, charset-valid slug "con", which
        // is then caught by the reserved-name check. "." or ".." would
        // instead sanitize to an empty string and hit the empty-result
        // error path tested separately above.
        let err = RadargramId::from_fallback("CON").unwrap_err();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn resolve_radargram_id_precedence() {
        // explicit wins over inherited and stem
        let (id, note) =
            resolve_radargram_id(Some("explicit-id"), Some("inherited-id"), "stem").unwrap();
        assert_eq!(id.as_str(), "explicit-id");
        assert!(note.is_none());

        // inherited wins over stem
        let (id, note) = resolve_radargram_id(None, Some("inherited-id"), "stem").unwrap();
        assert_eq!(id.as_str(), "inherited-id");
        assert!(note.is_none());

        // stem is the last resort, and is noted
        let (id, note) = resolve_radargram_id(None, None, "My Output Stem").unwrap();
        assert_eq!(id.as_str(), "my-output-stem");
        assert!(note.is_some());
    }

    #[test]
    fn resolve_radargram_id_rejects_invalid_explicit() {
        assert!(resolve_radargram_id(Some("Bad ID"), None, "stem").is_err());
    }

    #[test]
    fn display_name_empty_is_absent() {
        assert!(DisplayName::from_input("").is_none());
        assert!(DisplayName::from_input("   ").is_none());
        assert!(DisplayName::from_input(" Kroppbreen ").is_some());
    }

    #[test]
    fn display_name_allows_unicode_and_spaces() {
        let name = DisplayName::from_input("Kroppbreen sentrallinje nr 1 – øst").unwrap();
        assert_eq!(name.as_str(), "Kroppbreen sentrallinje nr 1 – øst");
    }

    #[test]
    fn resolve_display_name_precedence() {
        assert_eq!(
            resolve_display_name(Some("explicit"), Some("inherited")).map(|d| d.0),
            Some("explicit".to_string())
        );
        assert_eq!(
            resolve_display_name(None, Some("inherited")).map(|d| d.0),
            Some("inherited".to_string())
        );
        assert_eq!(resolve_display_name(None, None), None);
        // An explicit empty override does not fall through to "inherited";
        // per #116 the display name changing (including to absent) has no
        // identity semantics and is respected as given.
        assert_eq!(resolve_display_name(Some(""), Some("inherited")), None);
    }

    #[test]
    fn resolve_group_precedence_derives_id_from_name() {
        let (name, id) = resolve_group(Some("Dronbreen 2022"), None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(name.as_str(), "Dronbreen 2022");
        assert_eq!(id.as_str(), "dronbreen-2022");

        let (name, id) = resolve_group(None, None, Some("Dronbreen 2022"), None)
            .unwrap()
            .unwrap();
        assert_eq!(name.as_str(), "Dronbreen 2022");
        assert_eq!(id.as_str(), "dronbreen-2022");

        assert_eq!(resolve_group(None, None, None, None).unwrap(), None);
    }

    #[test]
    fn resolve_group_unicode_name_transliterates_to_ascii_id() {
        let (name, id) = resolve_group(Some("Drønbreen"), None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(name.as_str(), "Drønbreen");
        assert_eq!(id.as_str(), "dronbreen");
    }

    #[test]
    fn resolve_group_explicit_id_overrides_derivation() {
        let (name, id) = resolve_group(Some("Drønbreen"), Some("db"), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(name.as_str(), "Drønbreen");
        assert_eq!(id.as_str(), "db");
    }

    #[test]
    fn resolve_group_id_without_a_name_is_not_a_group() {
        // An id is only ever a derived byproduct of a name; a name-less id
        // has nothing to attach to.
        assert_eq!(
            resolve_group(None, Some("some-id"), None, None).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_group_rejects_invalid_explicit_id() {
        assert!(resolve_group(Some("Dronbreen"), Some("Bad Id"), None, None).is_err());
    }

    #[test]
    fn identifiers_sanitize_display_names() {
        assert_eq!(
            sanitize_to_identifier("Glacier bed (to temperate ice above)", &[]),
            "glacier_bed_to_temperate_ice_above"
        );
        assert_eq!(
            sanitize_to_identifier("Drønbreen bed", &[]),
            "dronbreen_bed"
        );
        assert_eq!(sanitize_to_identifier("2nd bed", &[]), "l_2nd_bed");
        assert_eq!(sanitize_to_identifier("!!!", &[]), "l_");
    }

    #[test]
    fn identifier_keywords_and_builtins_are_suffixed() {
        assert_eq!(sanitize_to_identifier("count", &[]), "count_layer");
        assert_eq!(sanitize_to_identifier("if", &[]), "if_layer");
        assert_eq!(sanitize_to_identifier("median", &[]), "median_layer");
        assert_eq!(sanitize_to_identifier("while", &[]), "while_layer");
    }

    #[test]
    fn identifier_collisions_get_a_numeric_suffix() {
        let taken = vec!["bed".to_string()];
        assert_eq!(sanitize_to_identifier("Bed", &taken), "bed_2");
        let taken = vec!["bed".to_string(), "bed_2".to_string()];
        assert_eq!(sanitize_to_identifier("Bed", &taken), "bed_3");
    }

    #[test]
    fn identifiers_are_valid_by_construction() {
        for name in [
            "Glacier bed (to temperate ice above)",
            "Drønbreen bed",
            "2nd bed",
            "!!!",
            "count",
            "if",
            "Ålesund — öst",
            "  leading and trailing  ",
        ] {
            let id = sanitize_to_identifier(name, &[]);
            assert!(is_valid_identifier(&id), "{name:?} produced {id:?}");
        }
    }

    #[test]
    fn identifier_validation_rejects_hyphens_and_other_slug_shapes() {
        assert!(!is_valid_identifier("bed-2"));
        assert!(is_valid_identifier("bed_2"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("_bed"));
        assert!(!is_valid_identifier("2bed"));
        assert!(!is_valid_identifier("Bed"));
        assert!(!is_valid_identifier("bed "));
    }

    #[test]
    fn radargram_id_and_group_id_are_distinct_types() {
        // Compile-time check: this would not compile if the macro produced
        // interchangeable types.
        fn takes_radargram_id(_: RadargramId) {}
        let id = RadargramId::new("a").unwrap();
        takes_radargram_id(id);
    }
}
