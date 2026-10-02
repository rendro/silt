//! Test suite: command-line front doors, the REPL, manifests, lockfiles and packages.
//!
//! One test binary; each module was a separate test crate before.

mod cli;
mod cli_add_tests;
mod cli_extra_positional_rejection_tests;
mod cli_fmt_idempotent_mtime_tests;
mod cli_global_flags_parity_tests;
mod cli_help_and_unknown_subcommand_tests;
mod cli_init_tests;
mod cli_positional_gate_tests;
mod cli_project_root_tests;
mod cli_round26_tests;
mod cli_round36_tests;
mod cli_self_update_flag_tests;
mod cli_test_rendering_tests;
mod cli_watch_subcommand_gate_tests;
mod empty_file_tests;
mod git_module_tests;
mod lockfile_tests;
mod manifest_git_url_security_tests;
mod manifest_tests;
mod manifest_unknown_fields_rejected_tests;
mod package_graph_lock_tests;
mod repl_completion_short_commands_tests;
mod repl_error_render_and_keywords_tests;
mod repl_keyword_parity_with_lexer_tests;
mod round73_cli_help_gaps_tests;
mod round75_silt_init_builtin_collision_tests;
mod round77_repl_enum_completion_tests;
mod round77_watch_initial_mtime_ordering_tests;
mod round79_install_sh_lock_tests;
mod round81_docs_watch_subcommands_parity_tests;
mod round87_cli_find_silt_files_skip_dirs_tests;
mod round91_watch_clear_tty_guard_tests;
mod round92_sigpipe_tests;
mod round96_help_watch_caveat_parity_tests;
mod run_banner_consistency_tests;
mod stage5_package_graph_tests;
mod trait_init_parity_tests;
mod watch;
mod watch_double_dash_separator_tests;
mod wave1_manifest_output_tests;
mod wave2_security_tests;
