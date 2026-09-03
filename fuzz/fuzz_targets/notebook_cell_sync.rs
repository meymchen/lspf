#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| lspf::fuzzing::notebook_cell_sync(data));
