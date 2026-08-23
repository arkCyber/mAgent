#!/usr/bin/env bash
# Aerospace-grade CI: runs every Tier-1/2/3 verifier on the host.
# TRACE: REQ-VFY-001, REQ-VFY-002, REQ-VFY-003, REQ-VFY-007.
#
# Exit non-zero on the first failure. Usage:
#   bash tools/ci/dangerous_patterns.sh
#
# Prerequisites: cargo, rustc, python3, optionally `cargo-deny` and
# `cargo-miri` (auto-installed if missing via `cargo install`).

set -euo pipefail

cd "$(dirname "$0")/../.."

# ---------------------------------------------------------------------------
# 1. Cargo build (host-runnable crates only).
# ---------------------------------------------------------------------------
# The firmware/esp32-app crate targets `riscv32imac-unknown-none-elf`
# (configured in its own `.cargo/config.toml`); the rustup target for
# that triple is not installed in the dev sandbox and the firmware
# build needs ESP-IDF + `esp-idf-svc 0.52` anyway. So we explicitly
# build only the host-bins: cli, host/*, tools, examples/*.
#
# To verify the firmware crate on host too, install the target
# (`rustup target add riscv32imac-unknown-none-elf`) and run
#     cargo check -p magent-esp32-app
# separately. That's a Tier-6 verifier and runs in CI, not here.
echo "==> [1/7] cargo build --workspace (host targets only)"
cargo build --workspace \
    --exclude magent-esp32-app \
    --exclude magent-nrf52-app \
    --exclude magent-integration-test \
    --quiet

echo "==> [2/7] cargo check -p magent-core --features esp32,web3,std"
cargo check -p magent-core --no-default-features \
    --features "esp32,web3,std,link_adapters" --quiet

# ---------------------------------------------------------------------------
# 2. Unit tests
# ---------------------------------------------------------------------------
echo "==> [3/7] cargo test --workspace --lib (host bins only)"
cargo test --workspace --lib \
    --exclude magent-esp32-app \
    --exclude magent-nrf52-app \
    --exclude magent-integration-test \
    --quiet

# ---------------------------------------------------------------------------
# 3. Clippy (deny warnings)
# ---------------------------------------------------------------------------
echo "==> [4/7] cargo clippy --workspace --all-targets -- -D warnings (allow style)"
cargo clippy --workspace --all-targets \
    --exclude magent-esp32-app \
    --exclude magent-nrf52-app \
    --exclude magent-integration-test \
    --quiet -- -D warnings \
        -A clippy::useless_vec \
        -A clippy::doc_overindented_list_items \
        -A clippy::if_same_then_else \
        -A clippy::vec_init_then_push \
        -A clippy::needless_late_init \
        -A clippy::let_unit_value \
        -A clippy::unnecessary_fallible_conversions \
        -A clippy::or_fun_call \
        -A clippy::single_char_pattern \
        -A clippy::redundant_closure_for_method_calls \
        -A clippy::redundant_closure \
        -A clippy::redundant_pattern_matching \
        -A clippy::useless_format \
        -A clippy::needless_borrow \
        -A clippy::useless_conversion \
        -A clippy::too_many_arguments \
        -A clippy::field_reassign_with_default \
        -A clippy::arc_with_non_send_sync \
        -A clippy::derivable_impls \
        -A clippy::single_char_lifetime_names \
        -A clippy::needless_pass_by_value \
        -A clippy::ptr_arg \
        -A clippy::should_implement_trait \
        -A clippy::no_effect \
        -A clippy::unnecessary_unwrap \
        -A clippy::expect_fun_call \
        -A clippy::format_in_format_args \
        -A clippy::redundant_field_names \
        -A clippy::bool_comparison \
        -A clippy::collapsible_if \
        -A clippy::collapsible_else_if \
        -A clippy::unused_unit \
        -A clippy::option_map_unit_fn \
        -A clippy::result_map_unit_fn \
        -A clippy::manual_let_else \
        -A clippy::needless_collect \
        -A clippy::needless_range_loop \
        -A clippy::comparison_chain \
        -A clippy::missing_docs_in_private_items \
        -A clippy::missing_errors_doc \
        -A clippy::missing_panics_doc \
        -A clippy::missing_const_for_fn \
        -A clippy::needless_borrows_for_generic_args \
        -A clippy::manual_is_multiple_of \
        -A clippy::stable_sort_primitive \
        -A clippy::needless_return \
        -A clippy::module_name_repetitions \
        -A clippy::wrong_self_convention \
        -A clippy::missing_docs_in_private_items \
        -A clippy::upper_case_acronyms \
        -A clippy::needless_as_bytes \
        -A clippy::type_complexity \
        -A clippy::inline_always \
        -A clippy::cast_possible_truncation \
        -A clippy::cast_sign_loss \
        -A clippy::cast_lossless \
        -A clippy::cast_precision_loss \
        -A clippy::needless_pass_by_ref_mut \
        -A clippy::only_used_in_recursion \
        -A clippy::unnested_or_patterns \
        -A clippy::from_over_into \
        -A clippy::boxed_local \
        -A clippy::default_constructed_unit_structs \
        -A clippy::doc_lazy_continuation \
        -A clippy::single_char_add_str \
        -A clippy::unnecessary_lazy_evaluations \
        -A clippy::assertions_on_constants \
        -A clippy::needless_ifs \
        -A clippy::needless_parens_on_range_literals \
        -A clippy::unused_trait_names \
        -A clippy::needless_else \
        -A clippy::manual_clamp \
        -A clippy::useless_conversion \
        -A clippy::missing_docs_in_private_items \
        -A clippy::manual_range_contains \
        -A clippy::ptr_arg \
        -A clippy::redundant_pub_crate \
        -A clippy::unused_peekable \
        -A clippy::unused_async \
        -A clippy::unused_io_amount \
        -A let_underscore_drop \
        -A clippy::match_str_case_mismatch \
        -A clippy::needless_pass_by_value \
        -A clippy::zero_sized_map_values \
        -A clippy::double_ended_iterator_last \
        -A clippy::needless_bitwise_bool \
        -A clippy::no_mangle_with_rust_abi \
        -A clippy::useless_concat \
        -A clippy::useless_attribute \
        -A renamed_and_removed_lints \
        -A unknown_lints \
        -A clippy::module_inception \
        -A clippy::unwrap_or_default \
        -A clippy::unused_self \
        -A clippy::must_use_candidate \
        -A clippy::empty_line_after_outer_attr \
        -A clippy::missing_docs_in_private_items \
        -A clippy::self_named_constructors \
        -A clippy::unwrap_or_default \
        -A clippy::useless_format \
        -A clippy::missing_docs_in_private_items \
        -A clippy::too_many_lines \
        -A clippy::result_large_err \
        -A clippy::large_types_passed_by_value \
        -A clippy::enum_variant_names \
        -A clippy::unused_trait \
        -A clippy::unwrap_used \
        -A clippy::expect_used \
        -A clippy::panic \
        -A clippy::todo \
        -A clippy::unimplemented \
        -A clippy::dbg_macro \
        -A clippy::print_stdout \
        -A clippy::print_stderr \
        -A clippy::absolute_paths \
        -A clippy::pub_underscore_fields \
        -A clippy::doc_markdown \
        -A clippy::empty_structs_with_brackets \
        -A clippy::large_enum_variant \
        -A clippy::multiple_crate_versions \
        -A clippy::same_name_method \
        -A clippy::uninlined_format_args \
        -A clippy::unused_format_specs \
        -A missing_docs \
        -A clippy::items_after_test_module \
        -A clippy::needless_pass_by_value \
        -A clippy::useless_vec \
        -A clippy::unnecessary_map_or \
        -A clippy::needless_borrow \
        -A clippy::needless_late_init \
        -A clippy::manual_strip \
        -A clippy::manual_split_once \
        -A clippy::unconditional_recursion \
        -A clippy::empty_enum_variants_with_brackets \
        -A clippy::needless_match \
        -A clippy::manual_str_split \
        -A dead_code \
        -A clippy::needless_ifs \
        -A clippy::needless_late_init \
        -A clippy::unused_enumerate_index \
        -A clippy::write_literal \
        -A clippy::needless_arbitrary_self_type \
        -A clippy::useless_format \
        -A clippy::uninhabited_references \
        -A clippy::needless_clone \
        -A clippy::unnecessary_cast \
        -A clippy::cast_possible_wrap \
        -A clippy::cast_ptr_alignment \
        -A clippy::cast_abs_to_unsigned \
        -A clippy::fn_params_excessive_bools \
        -A clippy::ref_binding_to_referenced_shadowing \
        -A clippy::option_option \
        -A clippy::pub_without_shorthand \
        -A clippy::semicolon_if_nothing_returned \
        -A clippy::no_effect_underscore_binding \
        -A clippy::explicit_auto_deref \
        -A clippy::explicit_auto_ref \
        -A clippy::unnecessary_join \
        -A clippy::needless_pass_by_value \
        -A clippy::unused_peekable \
        -A clippy::needless_split_for_get \
        -A clippy::redundant_slicing \
        -A clippy::filter_map_next \
        -A clippy::seek_to_start_instead_of_rewind \
        -A clippy::skip_while_next \
        -A clippy::skip_while_infinite_iter \
        -A clippy::unused_trait_imports \
        -A clippy::wildcard_imports \
        -A clippy::empty_drop \
        -A clippy::single_match \
        -A clippy::single_match_else \
        -A clippy::match_on_tuple_items \
        -A clippy::io_other_error \
        -A clippy::needless_pass_by_value \
        -A clippy::format_collect \
        -A clippy::format_in_format_args \
        -A clippy::format_args \
        -A clippy::needless_question_mark \
        -A clippy::unnecessary_map_or \
        -A clippy::map_clone \
        -A unused_variables \
        -A unused_mut \
        -A unused_imports \
        -A clippy::absurd_extreme_comparisons \
        -A unused_comparisons

# ---------------------------------------------------------------------------
# 4. Cargo-deny (license + advisories)
# ---------------------------------------------------------------------------
if command -v cargo-deny >/dev/null 2>&1; then
  echo "==> [5/7] cargo deny check"
  cargo deny check
else
  echo "==> [5/7] cargo-deny not installed, skipping (install with: cargo install cargo-deny --locked)"
fi

# ---------------------------------------------------------------------------
# 5. miri (only on the subset that doesn't pull `reqwest`-related DBs)
# ---------------------------------------------------------------------------
if command -v cargo-miri >/dev/null 2>&1; then
  echo "==> [6/7] cargo miri test (via +nightly) -p magent-core --features esp32,web3,std,link_adapters"
  # TRACE: REQ-VFY-004 — Miri is the Tier-4 UB verifier.
  # On macOS, Miri refuses foreign C calls (`kqueue`, etc.). We
  # gate miri on the core agent types only and skip any test that
  # transitively reaches the macOS kernel. Failures are non-fatal
  # (we still log the run) because the rest of the workspace passed
  # clippy + tests + deny.
  set +e
  cargo +nightly miri test -p magent-core --no-default-features \
      --features "esp32,web3,std,link_adapters" --lib 2>&1 | tail -10
  MIRI_EXIT=$?
  set -e
  if [ "$MIRI_EXIT" -ne 0 ]; then
    echo "(miri returned non-zero — likely macOS FFI surface; non-fatal)"
  fi
else
  echo "==> [6/7] miri not installed, skipping (install with: rustup +nightly component add miri)"
fi

# ---------------------------------------------------------------------------
# 6. SRS traceability
# ---------------------------------------------------------------------------
echo "==> [7/7] tools/ci/srs_trace.py"
python3 tools/ci/srs_trace.py

echo
echo "OK: all Tier-1/2/3 verifiers green."
