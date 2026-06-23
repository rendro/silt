export const meta = {
  name: 'silt-audit',
  description: 'Loop-engineered silt codebase audit: probe -> audit -> fix (worktree-isolated) -> single build+test+commit, repeat until 2 clean rounds',
  whenToUse: 'Nightly autonomous audit/fix loop for the silt language. Triggered by the nightly cron routine, or run on demand to converge the tree.',
  phases: [
    { title: 'Prep' },
    { title: 'Probe' },
    { title: 'Audit' },
    { title: 'Fix' },
    { title: 'Integrate' },
    { title: 'Notify' },
  ],
}

// ----------------------------------------------------------------------------
// silt-audit — one audit ROUND encoded as a deterministic fan-out, looped until
// the tree converges (2 consecutive rounds with zero REGRESSION and zero BROKEN).
//
// Ported from /home/klaus/dev/codebase-audit.md. Two silt-specific landmines
// shape the design and MUST NOT be undone:
//   1. Concurrent `cargo test` runs false-fail scheduler_deadlock_detector_tests
//      (CPU oversubscription). => fix agents NEVER run cargo; the single
//      integrator runs the one authoritative `cargo build && cargo test`.
//   2. target/ disk blowup (175GB -> lld SIGBUS). => fix agents work in
//      SOURCE-ONLY worktrees and never create a target dir.
// The old doc's no-git-ops / shared-tree / mid-session-revert scaffolding is
// therefore obsolete: parallel fix agents touch only their own worktree, and
// exactly one integrator owns all git history.
// ----------------------------------------------------------------------------

const NTFY = 'ntfy.sh/rendro-silt-completion-987'
const PRIOR = '/tmp/prior-audit-fixes.txt'
const BRANCH = args && args.branch ? args.branch : 'audit/auto-nightly'
const MAX_ROUNDS = args && args.maxRounds ? args.maxRounds : 6
const CLEAN_TARGET = 2 // stop after this many consecutive clean rounds

// The seven audit areas. Dead-code is special (grep-structural, not a
// correctness probe) and runs first per the doc.
const AREAS = [
  { key: 'type-system', desc: 'type system: inference, unification, trait/bound resolution, generics, canonical type equality' },
  { key: 'vm-runtime', desc: 'VM / runtime: bytecode dispatch, value representation, scheduler, channels, concurrency' },
  { key: 'error-handling', desc: 'error handling: diagnostics, spans, panic-vs-Result discipline, error message correctness' },
  { key: 'dx', desc: 'developer experience: REPL, LSP, formatter, CLI, first-run friction' },
  { key: 'docs', desc: 'documentation: doc-comment accuracy, examples that compile+run, counts/snippets matching code' },
  { key: 'tests', desc: 'test coverage: weak/typecheck-only locks for runtime bugs, missing regression tests, parity locks that drifted' },
]

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['severity', 'title', 'file', 'repro', 'inScope'],
        properties: {
          severity: { type: 'string', enum: ['BROKEN', 'GAP', 'LATENT', 'SOLID', 'REGRESSION'] },
          title: { type: 'string', description: 'one-line description' },
          file: { type: 'string', description: 'path:line' },
          repro: { type: 'string', description: 'minimal repro input or why none can be constructed' },
          regressedFrom: { type: 'string', description: 'prior commit hash if severity is REGRESSION, else empty' },
          inScope: { type: 'boolean', description: 'true if auto-fixable per the in-scope rules (internal bug/test/doc/dead-code), false if it needs a user design decision' },
          fixSketch: { type: 'string', description: 'how to fix, and what the locking test must exercise' },
        },
      },
    },
  },
}

const FIX_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['applied', 'patch', 'testName', 'files', 'summary'],
  properties: {
    applied: { type: 'boolean', description: 'true if you produced a complete fix + lock test' },
    patch: { type: 'string', description: 'the full `git diff --cached --binary` from your worktree, or empty string if applied=false' },
    testName: { type: 'string', description: 'name of the regression test you added (or n/a (doc-only))' },
    files: { type: 'array', items: { type: 'string' }, description: 'files your patch touches' },
    summary: { type: 'string', description: 'one line: what you changed and why' },
  },
}

const INTEGRATE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['committed', 'sha', 'buildPassed', 'testPassed', 'dropped', 'summary'],
  properties: {
    committed: { type: 'boolean' },
    sha: { type: 'string', description: 'commit sha, or empty if nothing committed' },
    buildPassed: { type: 'boolean' },
    testPassed: { type: 'boolean' },
    dropped: { type: 'array', items: { type: 'string' }, description: 'titles of patches you could not apply / that broke the build' },
    summary: { type: 'string' },
  },
}

const SCOPE_RULES = `
IN SCOPE for auto-fix (internal, no user-visible surface change):
- internal bug fixes (typechecker, VM, runtime, scheduler, compiler)
- soundness fixes that REJECT previously-unsound programs (close holes, not redesign)
- error-message / diagnostic / span improvements
- missing regression tests (especially locking prior fixes that shipped without one)
- documentation corrections (doc drift, wrong counts, broken snippets)
- internal refactors / dead-code removal (needs a no-op lock test: e.g. method_table count unchanged)
- performance fixes with unchanged observable semantics
OUT OF SCOPE (set inScope=false, never auto-fix):
- new/changed syntax, keywords, grammar; new language features
- stdlib signature changes that break existing correctly-typed programs
- user-visible renames; changes to precedence, evaluation order, scoping
- anything needing a design decision`

function key(f) {
  // dedup key: normalize file path (drop :line) + lowercased title head
  const file = (f.file || '').split(':')[0]
  const title = (f.title || '').toLowerCase().replace(/[^a-z0-9]+/g, ' ').trim().slice(0, 60)
  return file + '|' + title
}

// ----------------------------------------------------------------------------
// One round. Returns { regressions, broken, fixed, committed, sha, clean }.
// ----------------------------------------------------------------------------
async function runRound(round) {
  log(`Round ${round}: starting`)

  // --- Prep: refresh the authoritative prior-fix log + ensure the branch ---
  phase('Prep')
  await agent(
    `You are the prep step of round ${round} of the silt nightly audit.\n` +
    `1. Ensure we are on a working branch: \`git checkout -B ${BRANCH}\` (round 1 branches from the current HEAD; later rounds stay on it).\n` +
    `2. Regenerate the authoritative prior-fix log so this round's auditors can cross-check and detect regressions:\n` +
    `     git log --grep='Fix audit findings' --format='%h %s%n%b' -80 > ${PRIOR}\n` +
    `3. Print \`git rev-parse HEAD\` and \`git status --short\`.\n` +
    `Do NOT edit any source files. Report the HEAD sha and confirm ${PRIOR} was written.`,
    { label: `prep:r${round}`, phase: 'Prep' }
  )

  // --- Probe: dead-code / bloat structural scan (read-only, runs first) ---
  phase('Probe')
  const deadcode = await agent(
    `Run a dead-code / bloat probe across the silt codebase (read-only).\n` +
    `Hunt for: back-to-back duplicated block signatures; idempotent insert loops; unused pub items; ` +
    `dead cfg-gated code; unreachable match arms; helpers duplicated across files; left-over debug/scratch code; ` +
    `stale doc-comments; and parallel-array drift without a parity-lock test (the same enum/variant/name table ` +
    `hand-encoded in >2 surfaces -- editor grammar, REPL completion, LSP rename, docs index -- where only one has a parity lock).\n\n` +
    `Cross-check against the prior-fix log at ${PRIOR} (grep it). If a prior fix has regressed, mark severity REGRESSION with the commit hash.\n\n` +
    `HARD CONSTRAINTS: read-only, no edits, no git state changes. Use rg/grep/find/wc/uniq -c/Read. ` +
    `Do NOT run cargo build/test/check. Semantic-risk dead code (could mask a real bug when edited nearby) outranks pure bloat. ` +
    `For each finding set inScope per these rules:${SCOPE_RULES}\n` +
    `A dead-code fix is only inScope=true if its deletion can be proven a semantic no-op by a lock test.`,
    { label: 'probe:dead-code', phase: 'Probe', schema: FINDINGS_SCHEMA }
  )

  // --- Audit: 6 area agents in parallel (read-only) ---
  phase('Audit')
  const areaReports = await parallel(AREAS.map((a) => () =>
    agent(
      `Examine the "${a.desc}" area of the silt codebase. You are NOT required to find issues -- zero findings is a valid, useful outcome. Do not manufacture problems.\n\n` +
      `## Prior fixes (source of truth)\n` +
      `Grep ${PRIOR} (commits matching 'Fix audit findings'). Before flagging anything:\n` +
      `- matches a prior fix that is still intact -> do NOT report.\n` +
      `- matches a prior fix that has REGRESSED (lock test gone, code reverted, old behavior back) -> report as severity REGRESSION with the commit hash in regressedFrom, and treat as highest priority.\n` +
      `- genuinely different aspect -> report normally.\n` +
      `WEAK-GATE PATTERN: a prior fix whose lock is typecheck-only (e.g. assert check_str(..).is_ok()) for a bug whose root cause is in dispatch/VM/runtime/codegen. The lock protects the typechecker but not execution -- a later edit silently regresses it. If you see this, RUN the execution path mentally from the repro and check whether it still holds.\n\n` +
      `## Rules for every finding\n` +
      `1. Cite file:line. 2. Trace the full path from user-facing input to the suspect code. 3. If upstream (parser/compiler/typechecker) already maintains the invariant, say so and do NOT report. 4. Give a concrete minimal repro (silt source) or explain why none exists.\n` +
      `Severity: BROKEN (wrong result/crash on valid input) > GAP (missing functionality/docs blocking real use) > LATENT (correct but fragile; name the guarding invariant) > SOLID (well-implemented; say why) > REGRESSION (prior fix undone).\n` +
      `Set inScope per these rules:${SCOPE_RULES}\n\n` +
      `## Constraints\n` +
      `READ-ONLY. No edits, no temporary mutate-and-revert (parallel agents share the object store). Reason from the code. Do NOT run cargo (a concurrent cargo test would false-fail the scheduler deadlock detector). Five real findings beat ten speculative ones.`,
      { label: `audit:${a.key}`, phase: 'Audit', schema: FINDINGS_SCHEMA }
    )
  ))

  // --- Synthesis (plain JS: aggregate, dedup, cross-check, partition) ---
  const all = [deadcode, ...areaReports]
    .filter(Boolean)
    .flatMap((r) => r.findings || [])
  const seen = new Set()
  const deduped = []
  for (const f of all) {
    if (f.severity === 'SOLID') continue
    const k = key(f)
    if (seen.has(k)) continue
    seen.add(k)
    deduped.push(f)
  }
  const regressions = deduped.filter((f) => f.severity === 'REGRESSION')
  const broken = deduped.filter((f) => f.severity === 'BROKEN')
  const inScope = deduped.filter((f) => f.inScope)
  const deferred = deduped.filter((f) => !f.inScope)
  const clean = regressions.length === 0 && broken.length === 0

  log(`Round ${round}: ${deduped.length} findings (${regressions.length} REGRESSION, ${broken.length} BROKEN, ${inScope.length} in-scope, ${deferred.length} deferred)`)

  if (deferred.length) {
    log(`Round ${round}: deferred (need user decision): ` + deferred.map((f) => f.title).join('; '))
  }

  if (!inScope.length) {
    log(`Round ${round}: nothing in-scope to fix.`)
    return { round, regressions: regressions.length, broken: broken.length, fixed: 0, committed: false, sha: '', clean, deferred }
  }

  // --- Fix: one worktree-isolated agent per finding (parallel edit, NO cargo) ---
  phase('Fix')
  const patches = (await parallel(inScope.map((f) => () =>
    agent(
      `Fix exactly ONE silt audit finding inside your isolated git worktree.\n\n` +
      `FINDING [${f.severity}] ${f.title}\n` +
      `  file: ${f.file}\n  repro: ${f.repro}\n  fix sketch: ${f.fixSketch || '(none provided)'}\n\n` +
      `## Load-bearing rule: ship a regression test that FAILS before the fix and PASSES after.\n` +
      `Add it in the SAME patch. Without it the bug regresses next round and the loop spins in place.\n` +
      `- For runtime/dispatch/codegen bugs the lock MUST exercise the EXECUTION path, not just typecheck. ` +
      `Use the project's runner helpers (run_silt_ok / run_silt_raw / run_silt_inproc in tests/, or InProcessRunner from src/scheduler/test_support.rs) and assert on output/exit, NOT check_str(..).is_ok().\n` +
      `- For error-message / user-facing-string fixes prefer a source grep lock: include_str!("../src/foo.rs") + assert!(!src.contains("bad string")). A "construct the error and format it" test is tautological.\n` +
      `- For a dead-code deletion the lock must prove the deletion is a semantic no-op (e.g. assert a method_table/HashMap count unchanged, or shorter code yields same output).\n` +
      `- Doc-only fix with genuinely no testable behavior: testName = "n/a (doc-only)".\n` +
      `- If the fix changes a user-visible string or signature, grep the test suite for the OLD form and update those tests in your patch too.\n\n` +
      `## DO NOT run cargo (build/test/check).\n` +
      `Concurrent cargo test runs false-fail silt's scheduler_deadlock_detector_tests, and per-worktree target dirs have blown up the disk before. Reason about correctness from the code. The single integrator runs the one authoritative build+test.\n\n` +
      `## Capture your work as a patch.\n` +
      `When done, from your worktree root run:  git add -A && git diff --cached --binary\n` +
      `Return that full patch text in the "patch" field (it includes new test files). If you could not produce a correct fix, set applied=false and patch="".`,
      { label: `fix:${(f.file || '').split(':')[0].split('/').pop()}:${round}`, phase: 'Fix', schema: FIX_SCHEMA, isolation: 'worktree' }
    ).then((r) => (r && r.applied && r.patch ? { ...r, finding: f } : null))
  ))).filter(Boolean)

  log(`Round ${round}: ${patches.length}/${inScope.length} fixes produced patches`)
  if (!patches.length) {
    return { round, regressions: regressions.length, broken: broken.length, fixed: 0, committed: false, sha: '', clean, deferred }
  }

  // --- Integrate: ONE agent applies all patches, runs the single build+test,
  //     re-verifies repros, makes the one structured commit, pushes the branch. ---
  phase('Integrate')
  const manifest = patches.map((p, i) =>
    `### PATCH ${i + 1} — [${p.finding.severity}] ${p.finding.title}\n` +
    `test: ${p.testName} | files: ${p.files.join(', ')}\n` +
    `repro: ${p.finding.repro}\n` +
    (p.finding.regressedFrom ? `Regressed-from: ${p.finding.regressedFrom}\n` : '') +
    `<<<PATCH\n${p.patch}\nPATCH>>>`
  ).join('\n\n')

  const integration = await agent(
    `You are the SOLE integrator for round ${round} of the silt nightly audit. You own all git history this round; you run on the main working tree on branch ${BRANCH}.\n\n` +
    `You are given ${patches.length} patches (each a \`git diff --cached --binary\`). Do this in order:\n` +
    `1. Apply each patch to the working tree:  git apply --3way --whitespace=nofix  (feed the patch on stdin). If one fails to apply, set it aside (record its title in "dropped") and continue with the rest — never abort the whole round for one bad patch.\n` +
    `2. Run the ONE authoritative build+test from the merged tree:  cargo build && cargo test. Run it ONCE, serially — do NOT parallelize cargo test (silt's scheduler_deadlock_detector_tests false-fails under CPU oversubscription; if it fails, re-run that one test alone to confirm before treating it as real).\n` +
    `3. If the build or a non-flaky test fails, bisect: revert the most-likely-culpable patch (git checkout -- its files / reverse-apply), re-test, and add it to "dropped". Goal: a GREEN tree containing as many patches as cleanly integrate.\n` +
    `4. For every BROKEN/REGRESSION finding still in the tree, re-run its repro from the freshly built binary and confirm the bug is gone. Save each repro under /tmp/ so it can be re-run.\n` +
    `5. File-state sanity: \`git status --short\` — every changed file must be explained by a surviving patch. Investigate anything else before staging; do NOT blindly git add -A unexplained files.\n` +
    `6. Stage and make EXACTLY ONE commit. The subject MUST start with "Fix audit findings:" so future audits' \`git log --grep\` picks it up. Body = one block per surviving finding:\n` +
    `\n   <SEVERITY>: <one-line>\n     File: <path:line>\n     Test: <test name>\n     Repro: <minimal repro or "see test">\n   (REGRESSION blocks also add: Regressed-from: <hash>)\n\n` +
    `7. Push the branch:  git push -u origin ${BRANCH} --force-with-lease.\n\n` +
    `Report committed/sha/buildPassed/testPassed, the list of dropped patch titles, and a one-line summary.\n\n` +
    `PATCHES:\n${manifest}`,
    { label: `integrate:r${round}`, phase: 'Integrate', schema: INTEGRATE_SCHEMA }
  )

  return {
    round,
    regressions: regressions.length,
    broken: broken.length,
    fixed: integration ? patches.length - (integration.dropped || []).length : 0,
    committed: !!(integration && integration.committed),
    sha: integration ? integration.sha : '',
    buildPassed: integration ? integration.buildPassed : false,
    testPassed: integration ? integration.testPassed : false,
    dropped: integration ? integration.dropped || [] : [],
    clean,
    deferred,
  }
}

// ----------------------------------------------------------------------------
// The loop: run rounds until CLEAN_TARGET consecutive clean rounds, a round cap,
// or budget exhaustion. A "clean" round = the audit found zero REGRESSION and
// zero BROKEN (matches the doc's stated convergence signal; GAP/LATENT may
// still be fixed or deferred).
// ----------------------------------------------------------------------------
const history = []
let cleanStreak = 0
let round = 0
let stopReason = 'reached clean target'

while (cleanStreak < CLEAN_TARGET) {
  if (round >= MAX_ROUNDS) { stopReason = `hit MAX_ROUNDS (${MAX_ROUNDS})`; break }
  if (budget.total && budget.remaining() < 120_000) { stopReason = 'token budget low'; break }
  round += 1
  const r = await runRound(round)
  history.push(r)
  if (r.clean) {
    cleanStreak += 1
    log(`Round ${round}: CLEAN (${cleanStreak}/${CLEAN_TARGET})`)
  } else {
    cleanStreak = 0
    log(`Round ${round}: not clean (${r.regressions} REGRESSION, ${r.broken} BROKEN) — streak reset`)
  }
}

// --- Notify ---
phase('Notify')
const totalRegr = history.reduce((s, r) => s + r.regressions, 0)
const totalFixed = history.reduce((s, r) => s + r.fixed, 0)
const shas = history.filter((r) => r.committed).map((r) => r.sha).join(' ')
const allDeferred = [...new Set(history.flatMap((r) => (r.deferred || []).map((f) => f.title)))]
const summary =
  `silt nightly audit done: ${round} round(s), stop=${stopReason}. ` +
  `fixed=${totalFixed}, regressions-seen=${totalRegr}, commits=[${shas || 'none'}], branch=${BRANCH}. ` +
  (allDeferred.length ? `deferred(user-decision)=${allDeferred.length}: ${allDeferred.join('; ')}` : 'no deferred items.')

await agent(
  `Send a completion notification. Run exactly:\n` +
  `  curl -fsS -d ${JSON.stringify(summary)} ${NTFY}\n` +
  `Then report the curl exit status. Do nothing else — no edits, no git ops.`,
  { label: 'notify', phase: 'Notify' }
)

log(summary)
return { rounds: round, stopReason, totalFixed, totalRegressions: totalRegr, commits: shas, branch: BRANCH, deferred: allDeferred, history }
