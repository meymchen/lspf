//! The `notebook-cell-sync` fuzz target, run as an ordinary test (issue #254).
//!
//! A scheduled `ci/run-fuzz.sh` sweep reports only that it found nothing, and it
//! runs twice a week. These tests drive `lspf::fuzzing::notebook_cell_sync` with
//! the splice shapes that matter, so the target's own invariants — a refused
//! splice mutates nothing, an accepted one replaces exactly the deleted range,
//! and the cell index agrees with the committed cells — fail in milliseconds on
//! the push that broke them rather than days later. The `test` job in
//! `.github/workflows/ci.yml` runs this file explicitly, because the `fuzzing`
//! feature is deliberately outside the feature matrix.
//!
//! That an out-of-range splice is a *protocol error* is asserted where the error
//! value is visible: `notebooks::tests` for the store and `tests/notebook_sync.rs`
//! for the wire. What these tests add is the other half of the same claim, over a
//! far wider input space: no peer-controlled splice panics.
//!
//! Reading the seed corpus reaches outside the crate, so these tests run from
//! the workspace only. That is deliberate: the seeds are the fuzzer's entry
//! point, and nothing else would notice if one stopped decoding.

#![cfg(feature = "fuzzing")]

use std::path::Path;

use lspf::fuzzing::{NotebookSplice, notebook_cell_sync};

/// Cell names that make neighbouring slots share a URI, so a splice can drop a
/// cell whose URI a surviving cell still carries.
const ALIASED_NAMES: &[u8] = &[1, 1, 2, 2, 1, 1, 2];

fn splice(
    initial_count: usize,
    replacement_count: usize,
    start: u32,
    delete_count: u32,
) -> NotebookSplice {
    NotebookSplice {
        initial_count,
        replacement_count,
        start,
        delete_count,
    }
}

#[test]
fn every_committed_seed_decodes_to_the_outcome_its_name_claims() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/notebook-cell-sync");
    let mut seen = Vec::new();

    for entry in std::fs::read_dir(&corpus).expect("the committed seed corpus is present") {
        let path = entry.expect("the corpus entry is readable").path();
        let name = path
            .file_name()
            .expect("a corpus entry is a file")
            .to_string_lossy()
            .into_owned();
        let seed = std::fs::read(&path).expect("the seed is readable");

        notebook_cell_sync(&seed);

        // The target itself asserts that the notebook layer agrees with
        // `is_in_range`, so deciding the seed's outcome here needs only the
        // decode. A seed whose bytes stopped matching its prefix would
        // otherwise sit in the corpus teaching the fuzzer nothing.
        let (decoded, _) = NotebookSplice::decode(&seed);
        if let Some(expected_in_range) = name
            .starts_with("valid-")
            .then_some(true)
            .or_else(|| name.starts_with("malformed-").then_some(false))
        {
            assert_eq!(
                decoded.is_in_range(),
                expected_in_range,
                "seed {name} decodes to {decoded:?}, which its prefix contradicts"
            );
        }
        seen.push(name);
    }

    // `ci/run-fuzz.sh --check` refuses a target whose corpus lacks either
    // prefix. Failing here too names the seed that went missing, rather than
    // leaving it to a fuzz sweep that runs twice a week.
    for prefix in ["valid-", "malformed-"] {
        assert!(
            seen.iter().any(|name| name.starts_with(prefix)),
            "the seed corpus has no {prefix}* seed, only {seen:?}"
        );
    }
}

#[test]
fn splices_across_the_cell_array_boundary_hold_the_target_invariants() {
    for names in [&[][..], ALIASED_NAMES] {
        for initial_count in 0..=4usize {
            let len = initial_count as u32;
            for start in [0, 1, len.saturating_sub(1), len, len + 1, u32::MAX] {
                for delete_count in [0, 1, len.saturating_sub(1), len, len + 1, u32::MAX] {
                    for replacement_count in [0, 1, 3] {
                        let splice = splice(initial_count, replacement_count, start, delete_count);
                        // Both shapes: the exact encoding puts the indices on
                        // the boundary, and the narrowed one drives the fold
                        // that keeps a boundary splice reachable from raw bytes.
                        notebook_cell_sync(&splice.encode(names));
                        notebook_cell_sync(&splice.encode_narrowed(names));
                    }
                }
            }
        }
    }
}

#[test]
fn an_exact_encoding_round_trips_through_the_decoder() {
    for initial_count in [0, 1, 64] {
        for (start, delete_count) in [(0, 0), (1, 0), (0, u32::MAX), (u32::MAX, u32::MAX)] {
            let encoded = splice(initial_count, 3, start, delete_count).encode(ALIASED_NAMES);

            let (decoded, names) = NotebookSplice::decode(&encoded);

            assert_eq!(decoded, splice(initial_count, 3, start, delete_count));
            assert_eq!(
                names, ALIASED_NAMES,
                "the cell names survive the round trip"
            );
        }
    }
}

#[test]
fn an_input_shorter_than_the_controls_is_padded_rather_than_read_past_its_end() {
    let full = splice(3, 1, 1, 1).encode(&[7, 8, 9, 10]);

    for length in 0..=full.len() {
        notebook_cell_sync(&full[..length]);
    }
}

#[test]
fn an_input_past_the_target_limit_does_not_panic() {
    let mut oversized = splice(64, 64, 0, 0).encode(&[]);
    oversized.resize(1 << 20, 0);

    notebook_cell_sync(&oversized);
}
