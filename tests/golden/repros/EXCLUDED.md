# Programs of the repro archive that are not imported

The importer took every `.silt` file of the archive that contains `fn main`,
outside `work/frontend/mut/` (the formatter comment mutants, stage 8): 1,699
files (1,693 that grep reads as text, plus 6 it reads as binary). These are
left out, by archive path:

**Network: run would open sockets or connect to hosts (http/tcp/postgres)** (25)

- `work/architecture/feat/pg.silt`
- `work/architecture/feat/pg2.silt`
- `work/architecture/feat/pg3.silt`
- `work/architecture/feat/tls.silt`
- `work/concurrency/p10_iopool.silt`
- `work/concurrency/p12_iopool_cancel.silt`
- `work/concurrency/p13_deadline_zombie.silt`
- `work/concurrency/p40_http.silt`
- `work/concurrency/p41_tcp_duplex.silt`
- `work/concurrency/p41b_tcp_duplex_control.silt`
- `work/concurrency/p42_echo.silt`
- `work/concurrency/p67_deadline_main.silt`
- `work/concurrency/p67b_deadline_task.silt`
- `work/frontend/docs/README_4.silt`
- `work/frontend/ex/cross_module_errors.silt`
- `work/frontend/ex/http_client.silt`
- `work/frontend/ex/http_server.silt`
- `work/frontend/exfmt/cross_module_errors.silt`
- `work/frontend/exfmt/http_client.silt`
- `work/frontend/exfmt/http_server.silt`
- `work/stdlib/t/pg.silt`
- `work/test-arch/run/ex/cross_module_errors.silt`
- `work/test-arch/run/ex/http_client.silt`
- `work/test-arch/run/ex/http_server.silt`
- `work/verify/ts/sl8.silt`

**Not UTF-8: an LSP client cannot send the text, so the LSP door cannot see it** (3)

- `work/frontend/deep/invalid_utf8.silt`
- `work/frontend/deep/invalid_utf8_code.silt`
- `work/tooling/cli/latin1.silt`

**Git dependency: needs a git repository or the network (and the archive's ones are argument-injection probes with paths of the audit machine)** (4)

- `work/packages/g1/app/src/main.silt`
- `work/packages/g2/app/src/main.silt`
- `work/packages/g3/app/src/main.silt`
- `work/verify/g2/app/src/main.silt`

**Timing: `silt check` takes more than 5 s in a debug build** (28). These are
performance repros (deep nesting, thousands of locals or functions,
exhaustiveness blowups); their verdict would depend on the machine's speed
and the 20 s case timeout. Measured with four checks at once; listed by the
case name they would have had, with the time:

- `type_soundness/deep9_24.silt` (60.7 s, timed out at 60 s)
- `type_soundness/deep9.silt` (60.6 s, timed out at 60 s)
- `type_soundness/deep9_22.silt` (60.3 s, timed out at 60 s)
- `frontend/deep_trait_method_nest.silt` (60.0 s, timed out at 60 s)
- `frontend/deep_decls_many.silt` (60.0 s, timed out at 60 s)
- `compiler_vm/limits_consts70000.silt` (60.0 s, timed out at 60 s)
- `frontend/deep_dm.silt` (60.0 s, timed out at 60 s)
- `compiler_vm/limits_locals66000.silt` (60.0 s, timed out at 60 s)
- `frontend/deep_ls.silt` (40.9 s)
- `typechecker_arch/perf_p800.silt` (33.3 s)
- `frontend/deep_lambda_nest.silt` (20.1 s)
- `type_soundness/deep9_20.silt` (19.8 s)
- `compiler_vm/limits_locals8000.silt` (17.3 s)
- `compiler_vm/limits_fns2000.silt` (17.2 s)
- `verify/arch_perf_eight` (16.0 s)
- `concurrency/p90_spawn_big.silt` (15.7 s)
- `typechecker_arch/perf_p400.silt` (13.8 s)
- `frontend/deep_pattern_or.silt` (11.7 s)
- `typechecker_arch/perfmod_main10` (11.3 s)
- `compiler_vm/limits_fns1000.silt` (8.2 s)
- `compiler_vm/limits_locals4000.silt` (7.1 s)
- `tooling/perf_six` (7.0 s)
- `compiler_vm/limits_match6000.silt` (7.0 s)
- `typechecker_arch/perf_p200.silt` (6.5 s)
- `packages/t7_main` (5.9 s)
- `typechecker_arch/perfmod_main5` (5.9 s)
- `type_soundness/deep9_18.silt` (5.3 s)
- `architecture/perf_eight` (5.0 s)
