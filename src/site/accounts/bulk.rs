//! Creating accounts in batches, for a class or a workshop.
//!
//! Numbered names from a prefix, or names drawn from a fixed pool, and
//! generated passwords for the case where invite links are impractical.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "reached through the server's HTTP routes; a CLI-only build \
                  still needs the types for `ridal site` and `ridal project`"
    )
)]

use std::fmt;

use super::MIN_PASSWORD_LEN;
use crate::identity::UserId;
use crate::project::roles::Role;

/// The largest account batch accepted by the browser and command line.
pub const MAX_BULK_ACCOUNTS: usize = 100;

/// Generated passwords avoid characters that are easy to confuse on paper.
const GENERATED_PASSWORD_ALPHABET: &[u8] =
    b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";

/// The pool drawn from when an administrator asks for random accounts
/// instead of a numbered prefix (#202).
///
/// Hardcoded rather than generated so a workshop's names are the same every
/// time. Each source name is lowercase and hyphen-separated because an
/// account name is a slug -- it is a filename inside the project and a path
/// component in URLs, and [`UserId`] accepts only `[a-z0-9_-]`. Hyphens
/// rather than underscores so these read like the ids [`sanitize_to_slug`]
/// derives for radargrams and groups.
///
/// Shorter than [`MAX_BULK_ACCOUNTS`], which is deliberate: a batch larger
/// than the unused part of this pool is refused with a pointer at prefixes,
/// rather than handed a partial set of names.
pub const RANDOM_USERNAMES: [&str; 59] = [
    "ice-wanderer",
    "ping-prankster",
    "arctic-ally",
    "ice-whiz",
    "chirp-cheer",
    "snow-surfer",
    "radar-rover",
    "polar-punk",
    "avalanche-amigo",
    "signal-scout",
    "signal-sled",
    "polar-partier",
    "radar-rowdy",
    "snow-scout",
    "ice-crewmate",
    "radar-rebel",
    "crevasse-clan",
    "polar-pioneer",
    "ice-cap-cohort",
    "glacier-grin",
    "wave-wizard",
    "berg-buddy",
    "signal-syncer",
    "team-freezer",
    "glacial-guru",
    "frosty-buddy",
    "chirp-chum",
    "ping-pioneer",
    "echo-enthusiast",
    "signal-swoop",
    "glacier-pal",
    "snowy-scout",
    "echo-explorer",
    "chill-chum",
    "field-frost",
    "snow-chum",
    "frosty-frolic",
    "cheer-chill",
    "radar-rifter",
    "ping-frost",
    "frosty-folk",
    "wave-wanderer",
    "radar-scout",
    "frosty-fellow",
    "radar-riff",
    "melt-mate",
    "signal-mate",
    "radar-force",
    "frosty-fusion",
    "frosty-friend",
    "frost-facet",
    "polar-pal",
    "glacier-goof",
    "crew-freeze",
    "blizzard-buddy",
    "berg-bard",
    "snow-frolic",
    "echo-teammate",
    "chill-chaser",
];

/// The warning shown when an administrator chooses shared, pre-generated
/// credentials instead of one-time invite links.
///
/// Named for the risk it describes rather than the secret it is about: it
/// returns advice, not a credential, and a name containing "password" made
/// CodeQL's name-based sensitive-data heuristic treat every advisory string
/// as a secret (#224).
pub fn bulk_risk_advisory(role: Role) -> Option<&'static str> {
    match role {
        Role::Viewer => Some("This is suboptimal, but acceptable for a small teaching session."),
        Role::Picker => Some("Next time, please consider invite links: they are less prone to issues."),
        Role::Operator => Some(
            "This is generally ill-advised: anyone who gets this list can download or delete real data.",
        ),
        Role::Admin => None,
    }
}

/// Generate a batch of zero-padded names beginning at `start`, inclusive.
pub fn bulk_names_after(
    prefix: &str,
    count: usize,
    start: usize,
) -> Result<Vec<UserId>, BulkError> {
    if count == 0 || count > MAX_BULK_ACCOUNTS {
        return Err(BulkError(format!(
            "A bulk operation must contain between 1 and {MAX_BULK_ACCOUNTS} accounts."
        )));
    }
    let width = count.to_string().len().max(2);
    if start == 0 {
        return Err(BulkError(
            "A bulk account sequence must start at 1.".to_string(),
        ));
    }
    (start..start + count)
        .map(|index| UserId::new(format!("{prefix}-{index:0width$}")).map_err(BulkError))
        .collect()
}

/// Pick `count` distinct random usernames, none of them in `used`.
///
/// Uniform without replacement: the unused part of [`RANDOM_USERNAMES`] is
/// partially shuffled and its first `count` entries taken. When too few names
/// remain the whole draw is refused, naming the shortfall and pointing at
/// prefixes, so an administrator is never handed a smaller batch than asked
/// for without being told.
pub fn random_bulk_names<'a>(
    used: impl Iterator<Item = &'a UserId>,
    count: usize,
) -> Result<Vec<UserId>, BulkError> {
    if count == 0 || count > MAX_BULK_ACCOUNTS {
        return Err(BulkError(format!(
            "A bulk operation must contain between 1 and {MAX_BULK_ACCOUNTS} accounts."
        )));
    }
    let used: std::collections::HashSet<&str> = used.map(UserId::as_str).collect();
    let mut available: Vec<&str> = RANDOM_USERNAMES
        .iter()
        .copied()
        .filter(|name| !used.contains(name))
        .collect();
    if count > available.len() {
        return Err(BulkError(format!(
            "Only {} random usernames are still unused, so {count} cannot be \
             drawn. Add the rest with a prefix instead, for example prefix \
             'student'.",
            available.len()
        )));
    }

    let remaining = available.len();
    for index in 0..count {
        let swap_with = index + random_below(remaining - index)?;
        available.swap(index, swap_with);
    }
    available[..count]
        .iter()
        .map(|name| UserId::new(*name).map_err(BulkError))
        .collect()
}

/// A uniform `usize` in `0..limit`, by rejection so a modulo cannot bias it.
fn random_below(limit: usize) -> Result<usize, BulkError> {
    if limit <= 1 {
        return Ok(0);
    }
    let limit = limit as u64;
    // Values below this are a whole multiple of `limit`; the narrow band above
    // it is discarded rather than folding unevenly onto the low numbers.
    let ceiling = u64::MAX - (u64::MAX % limit);
    loop {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|e| BulkError(format!("could not read system randomness: {e}")))?;
        let value = u64::from_le_bytes(bytes);
        if value < ceiling {
            return Ok((value % limit) as usize);
        }
    }
}

/// Generate one printable password without ambiguous look-alike characters.
pub fn generate_password() -> Result<String, BulkError> {
    let mut password = String::with_capacity(MIN_PASSWORD_LEN);
    let alphabet_len = GENERATED_PASSWORD_ALPHABET.len();
    let limit = (256 / alphabet_len) * alphabet_len;
    while password.len() < MIN_PASSWORD_LEN {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| BulkError(format!("could not read system randomness: {e}")))?;
        for byte in bytes {
            if byte as usize >= limit {
                continue;
            }
            password.push(GENERATED_PASSWORD_ALPHABET[byte as usize % alphabet_len] as char);
            if password.len() == MIN_PASSWORD_LEN {
                break;
            }
        }
    }
    Ok(password)
}

/// Return the first suffix after every existing `{prefix}-N` name in `used`.
pub fn next_bulk_start<'a>(used: impl Iterator<Item = &'a UserId>, prefix: &str) -> usize {
    used.filter_map(|name| {
        name.as_str()
            .strip_prefix(&format!("{prefix}-"))
            .and_then(|suffix| suffix.parse::<usize>().ok())
    })
    .max()
    .unwrap_or(0)
        + 1
}

/// Why a batch could not be made: a bad size or prefix, too few names left
/// in the pool, or no system randomness.
#[derive(Debug)]
pub struct BulkError(pub String);

impl fmt::Display for BulkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BulkError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken(names: &[&str]) -> Vec<UserId> {
        names
            .iter()
            .map(|name| UserId::new(*name).unwrap())
            .collect()
    }

    #[test]
    fn bulk_names_are_zero_padded_and_bounded() {
        let names = bulk_names_after("student", 3, 1).unwrap();
        assert_eq!(
            names.iter().map(UserId::as_str).collect::<Vec<_>>(),
            ["student-01", "student-02", "student-03"]
        );
        assert!(bulk_names_after("student", 0, 1).is_err());
        assert!(bulk_names_after("student", MAX_BULK_ACCOUNTS + 1, 1).is_err());
        assert_eq!(
            bulk_names_after("student", 2, 4)
                .unwrap()
                .iter()
                .map(UserId::as_str)
                .collect::<Vec<_>>(),
            ["student-04", "student-05"]
        );
    }

    #[test]
    fn generated_passwords_are_readable_and_valid() {
        let first = generate_password().unwrap();
        let second = generate_password().unwrap();
        assert_eq!(first.len(), MIN_PASSWORD_LEN);
        assert!(first
            .chars()
            .all(|c| { GENERATED_PASSWORD_ALPHABET.contains(&(c as u8)) }));
        assert_ne!(first, second);
        crate::site::accounts::check_password(&first).unwrap();
    }

    #[test]
    fn bulk_passwords_are_disallowed_for_admins() {
        assert!(bulk_risk_advisory(Role::Admin).is_none());
        assert!(bulk_risk_advisory(Role::Viewer).is_some());
        assert!(bulk_risk_advisory(Role::Picker).unwrap().contains("invite"));
    }

    #[test]
    fn random_usernames_are_distinct_and_avoid_taken_ones() {
        let used = taken(&["ice-wanderer"]);
        let names = random_bulk_names(used.iter(), 5).unwrap();
        assert_eq!(names.len(), 5);
        let unique: std::collections::HashSet<&str> = names.iter().map(UserId::as_str).collect();
        assert_eq!(unique.len(), 5, "a draw must not repeat a name");
        assert!(
            !unique.contains("ice-wanderer"),
            "a drawn name must be free"
        );
        for name in &names {
            assert!(RANDOM_USERNAMES.contains(&name.as_str()), "{name}");
        }
    }

    #[test]
    fn random_usernames_refuse_more_than_the_pool_holds() {
        let error = random_bulk_names(std::iter::empty(), RANDOM_USERNAMES.len() + 1).unwrap_err();
        assert!(error.to_string().contains("prefix"), "{error}");

        // Exhausted pool: nothing left to draw, and the message says which
        // way out is available rather than returning a short batch.
        let full = taken(&RANDOM_USERNAMES);
        let error = random_bulk_names(full.iter(), 1).unwrap_err();
        assert!(error.to_string().contains("prefix"), "{error}");
    }
}
