//! The `notebook_cell_sync` fuzz target, run as an ordinary test (issue #254).
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

/// One target input, in the layout `lspf::fuzzing::notebook_cell_sync` decodes.
/// `shape` picks how `start` and `delete_count` are read: even narrows them to
/// the cell array, odd takes them whole. One name byte per cell slot follows,
/// and repeating a byte makes two slots share a cell URI.
fn input(
    initial: u8,
    replacement: u8,
    shape: u8,
    start: u32,
    delete_count: u32,
    names: &[u8],
) -> Vec<u8> {
    let mut bytes = vec![initial, replacement, shape];
    bytes.extend_from_slice(&start.to_le_bytes());
    bytes.extend_from_slice(&delete_count.to_le_bytes());
    bytes.extend_from_slice(names);
    bytes
}

#[test]
fn every_committed_seed_replays_cleanly() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/notebook_cell_sync");
    let mut names = Vec::new();

    for entry in std::fs::read_dir(&corpus).expect("the committed seed corpus is present") {
        let path = entry.expect("the corpus entry is readable").path();
        let seed = std::fs::read(&path).expect("the seed is readable");
        lspf::fuzzing::notebook_cell_sync(&seed);
        names.push(
            path.file_name()
                .expect("a corpus entry is a file")
                .to_string_lossy()
                .into_owned(),
        );
    }

    // `ci/run-fuzz.sh --check` refuses a target whose corpus lacks either
    // prefix. Failing here too names the seed that went missing, rather than
    // leaving it to a fuzz sweep that runs twice a week.
    for prefix in ["valid-", "malformed-"] {
        assert!(
            names.iter().any(|name| name.starts_with(prefix)),
            "the seed corpus has no {prefix}* seed, only {names:?}"
        );
    }
}

#[test]
fn splices_across_the_cell_array_boundary_hold_the_target_invariants() {
    // Distinct names give every cell its own URI; the repeating pattern makes
    // cells alias, so a splice can drop a cell whose URI a surviving cell still
    // carries. Both shapes are swept: narrowed indices land on the boundary,
    // whole ones reach the values that would overflow the range check.
    for names in [&[][..], &[1, 1, 2, 2, 1, 1, 2][..]] {
        for shape in [0, 1] {
            for cells in 0..=4u8 {
                let len = u32::from(cells);
                for start in [0, 1, len.saturating_sub(1), len, len + 1, u32::MAX] {
                    for delete_count in [0, 1, len.saturating_sub(1), len, len + 1, u32::MAX] {
                        for replacement in [0, 1, 3] {
                            lspf::fuzzing::notebook_cell_sync(&input(
                                cells,
                                replacement,
                                shape,
                                start,
                                delete_count,
                                names,
                            ));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn an_input_shorter_than_the_controls_is_padded_rather_than_read_past_its_end() {
    let full = input(3, 1, 0, 1, 1, &[7, 8, 9, 10]);

    for length in 0..=full.len() {
        lspf::fuzzing::notebook_cell_sync(&full[..length]);
    }
}

#[test]
fn an_input_past_the_target_limit_does_not_panic() {
    let mut oversized = input(255, 255, 0, 0, 0, &[]);
    oversized.resize(1 << 20, 0);

    lspf::fuzzing::notebook_cell_sync(&oversized);
}
