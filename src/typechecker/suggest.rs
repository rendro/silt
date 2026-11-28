//! "did you mean ...?" suggestion helper for type-checker diagnostics.
//!
//! When the type checker emits
//! `undefined variable 'TYPO'` or `module 'MOD' has no member 'TYPO'`,
//! it should append a `did you mean \`CANDIDATE\`?` hint when
//! a close-enough identifier exists in scope. This module owns the
//! threshold policy so call sites stay one-line; the distance is
//! [`crate::text::levenshtein`], which the command line's hint uses too.

use crate::text::levenshtein;

/// Return the closest candidate to `typo` from `candidates`, or `None`
/// if no candidate is close enough.
///
/// Threshold policy:
/// - Exact matches to `typo` are filtered out (never suggest yourself).
/// - For a pair (typo, candidate), define `max = typo.len().max(cand.len())`.
/// - Short pairs (`max <= 5`) accept Levenshtein distance `<= 1`.
///   (`<= 2` would give low-signal hits like `foo` → `Bool`,
///   d=2.) Genuine 1-edit typos
///   on very short names still pass (`fo` → `for`, `namee` → `name`).
///   Two-edit typos on slightly longer names remain covered by the
///   scaled rule below: `pintln` → `println` (d=1, max=7, 1*3 <= 7),
///   `lenght` → `length` (d=2, max=6, 2*3 <= 6).
/// - Longer pairs accept `d * 3 <= max`, i.e. up to ~33% of the longer
///   string may change. Scales with length so 20-character names don't
///   get stuck at the absolute-1 threshold.
///
/// Among accepted candidates the smallest edit distance wins; ties break
/// lexicographically to keep the hint deterministic. A candidate that
/// differs from `typo` only in case and underscores (`Y` and `y`,
/// `MaxSize` and `max_size`) is the name meant: it wins before any
/// distance is counted.
pub(crate) fn suggest_similar<I, S>(typo: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let folded = |name: &str| -> String {
        (name.chars())
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect()
    };
    let typo_folded = folded(typo);
    let mut best: Option<(usize, String)> = None;
    let mut same: Option<String> = None;
    for c in candidates {
        let c = c.as_ref();
        if c == typo {
            // Never suggest the typo itself — this can happen when the
            // caller sweeps its own env and the name is present under
            // a different scope (e.g. a shadowed fn param).
            continue;
        }
        if !typo_folded.is_empty() && folded(c) == typo_folded {
            if same.as_ref().is_none_or(|s| c < s.as_str()) {
                same = Some(c.to_string());
            }
            continue;
        }
        let d = levenshtein(typo, c);
        let max = typo.chars().count().max(c.chars().count());
        let accept = if max <= 5 {
            d <= 1
        } else {
            d.saturating_mul(3) <= max
        };
        if !accept {
            continue;
        }
        match &best {
            Some((bd, bc)) if *bd < d => {}
            Some((bd, bc)) if *bd == d && bc.as_str() < c => {}
            _ => best = Some((d, c.to_string())),
        }
    }
    same.or(best.map(|(_, c)| c))
}

#[cfg(test)]
mod tests;
