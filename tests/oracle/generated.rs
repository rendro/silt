//! Generated programs as inputs of the oracle, with a reference
//! evaluator: `main` is one expression of a small subset whose every
//! form has the type `Int`, so each program checks clean by
//! construction, and what it returns (a number, or the integer overflow
//! it stops at) is known without the VM ([`eval`]).
//!
//! The forms: number literals, variables, `list.length` of a list
//! literal, `+ - *`, a `match` on a comparison, `let`, a `match` with an
//! or-pattern over a tuple, a closure called where it is written, a
//! pipe into a closure, and a `let` that takes a tuple apart.
//!
//! A program is made from a seed and its number alone, and is named by
//! them (`generated/<seed>/<number>`): a finding is found again by its
//! name, and `SILT_ORACLE_SEED=<n>` draws other programs. A finding is
//! shown with the smallest program that still has it ([`minimise`]).

use silt::Value;

use crate::oracle::{Expect, Finding, Input, Source, Verdict, examine};
use crate::sweep::{check_listed, chosen, conclude, full, run, skips, steps};

/// How many programs a run generates.
const PROGRAMS: usize = 400;
const PROGRAMS_FULL: usize = 10_000;

/// The seed of the programs, unless `SILT_ORACLE_SEED` gives one.
const SEED: u64 = 1;

/// How deep the expression of a program nests, at most.
const MAX_DEPTH: u32 = 5;

/// An expression of the subset. Every one has the type `Int`.
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// `(n)`
    Lit(i64),
    /// `(name)`
    Var(String),
    /// `list.length([(0), (1), ...])` of so many elements
    ListLength(u8),
    /// `(a op b)`
    Binary(Box<Expr>, char, Box<Expr>),
    /// `match ((cond) > 0) { true -> then  false -> otherwise }`
    IfPositive {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// `{ let name = bound  body }`
    Let {
        name: String,
        bound: Box<Expr>,
        body: Box<Expr>,
    },
    /// `match (a, b) { (0, _) | (_, 0) -> hit  _ -> miss }`
    EitherZero {
        a: Box<Expr>,
        b: Box<Expr>,
        hit: Box<Expr>,
        miss: Box<Expr>,
    },
    /// `{ param -> body }(arg)`
    Call {
        param: String,
        arg: Box<Expr>,
        body: Box<Expr>,
    },
    /// `seed |> { v -> v + bias }`
    Pipe { seed: Box<Expr>, bias: Box<Expr> },
    /// `{ let (u, v) = (a, b)  u + v }`
    TupleLet { a: Box<Expr>, b: Box<Expr> },
}

// ── The reference evaluator ─────────────────────────────────────────

/// `a op b` as silt computes it: the number, or the message of the
/// runtime error an overflow is.
fn arithmetic(a: i64, op: char, b: i64) -> Result<i64, String> {
    let result = match op {
        '+' => a.checked_add(b),
        '-' => a.checked_sub(b),
        '*' => a.checked_mul(b),
        _ => unreachable!("the subset has + - *"),
    };
    result.ok_or_else(|| format!("integer overflow: {a} {op} {b}"))
}

/// The value of `expr` where `env` holds the variables in scope, the
/// innermost last: its number, or the message of the runtime error it
/// stops at. Operands are evaluated from left to right, and a branch
/// only when it is taken.
fn eval(expr: &Expr, env: &mut Vec<(String, i64)>) -> Result<i64, String> {
    // `body` with `name` bound to `value`.
    fn bound(
        name: &str,
        value: i64,
        body: &Expr,
        env: &mut Vec<(String, i64)>,
    ) -> Result<i64, String> {
        env.push((name.to_string(), value));
        let result = eval(body, env);
        env.pop();
        result
    }
    match expr {
        Expr::Lit(n) => Ok(*n),
        Expr::Var(name) => {
            let found = env.iter().rev().find(|(bound, _)| bound == name);
            Ok(found.expect("the generator names variables in scope").1)
        }
        Expr::ListLength(n) => Ok(i64::from(*n)),
        Expr::Binary(a, op, b) => {
            let a = eval(a, env)?;
            let b = eval(b, env)?;
            arithmetic(a, *op, b)
        }
        Expr::IfPositive {
            cond,
            then,
            otherwise,
        } => match eval(cond, env)? > 0 {
            true => eval(then, env),
            false => eval(otherwise, env),
        },
        Expr::Let {
            name,
            bound: value,
            body,
        } => {
            let value = eval(value, env)?;
            bound(name, value, body, env)
        }
        Expr::EitherZero { a, b, hit, miss } => {
            let a = eval(a, env)?;
            let b = eval(b, env)?;
            match a == 0 || b == 0 {
                true => eval(hit, env),
                false => eval(miss, env),
            }
        }
        Expr::Call { param, arg, body } => {
            let arg = eval(arg, env)?;
            bound(param, arg, body, env)
        }
        // The closure's `v` is in scope where the bias is evaluated.
        Expr::Pipe { seed, bias } => {
            let seed = eval(seed, env)?;
            let bias = bound("v", seed, bias, env)?;
            arithmetic(seed, '+', bias)
        }
        Expr::TupleLet { a, b } => {
            let a = eval(a, env)?;
            let b = eval(b, env)?;
            arithmetic(a, '+', b)
        }
    }
}

// ── The program text ────────────────────────────────────────────────

/// `expr` as silt source, its lines after the first indented by
/// `indent` levels.
fn render(expr: &Expr, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner = |expr: &Expr| render(expr, indent + 1);
    match expr {
        Expr::Lit(n) => format!("({n})"),
        Expr::Var(name) => format!("({name})"),
        Expr::ListLength(n) => {
            let items: Vec<String> = (0..*n).map(|i| format!("({i})")).collect();
            format!("list.length([{}])", items.join(", "))
        }
        Expr::Binary(a, op, b) => format!("({} {op} {})", render(a, indent), render(b, indent)),
        Expr::IfPositive {
            cond,
            then,
            otherwise,
        } => format!(
            "(match (({}) > 0) {{\n{pad}  true -> ({})\n{pad}  false -> ({})\n{pad}}})",
            inner(cond),
            inner(then),
            inner(otherwise)
        ),
        Expr::Let { name, bound, body } => format!(
            "{{\n{pad}  let {name} = ({})\n{pad}  ({})\n{pad}}}",
            inner(bound),
            inner(body)
        ),
        Expr::EitherZero { a, b, hit, miss } => format!(
            "(match (({}), ({})) {{\n{pad}  (0, _) | (_, 0) -> ({})\n{pad}  _ -> ({})\n{pad}}})",
            inner(a),
            inner(b),
            inner(hit),
            inner(miss)
        ),
        Expr::Call { param, arg, body } => format!(
            "({{ {param} -> ({}) }}({}))",
            render(body, indent),
            render(arg, indent)
        ),
        Expr::Pipe { seed, bias } => format!(
            "(({}) |> {{ v -> v + ({}) }})",
            render(seed, indent),
            render(bias, indent)
        ),
        Expr::TupleLet { a, b } => format!(
            "{{\n{pad}  let (u, v) = (({}), ({}))\n{pad}  (u + v)\n{pad}}}",
            inner(a),
            inner(b)
        ),
    }
}

/// The program whose `main` is `expr`.
fn program(expr: &Expr) -> String {
    format!("import list\n\nfn main() {{\n  {}\n}}\n", render(expr, 1))
}

// ── The generator ───────────────────────────────────────────────────

/// A sequence of numbers that its seed decides (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `n`.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The names the generator must not give a variable: keywords, and the
/// builtin modules.
const TAKEN: &[&str] = &[
    "fn", "let", "type", "trait", "impl", "match", "when", "return", "pub", "import", "as", "else",
    "where", "loop", "true", "false", "mod", "main", "list", "string", "map", "set", "int",
    "float", "io", "fs", "env", "test", "regex", "json", "toml", "math", "channel", "task", "time",
    "http", "bytes", "tcp", "stream", "result", "option", "uuid",
];

/// A variable name: one to three letters, and a digit now and then.
fn name(rng: &mut Rng) -> String {
    loop {
        let mut name = String::new();
        for _ in 0..=rng.below(3) {
            name.push((b'a' + rng.below(26) as u8) as char);
        }
        if rng.below(4) == 0 {
            name.push((b'0' + rng.below(10) as u8) as char);
        }
        if !TAKEN.contains(&name.as_str()) {
            return name;
        }
    }
}

/// Numbers near the ends of `Int`: with them a sum or a product
/// overflows now and then.
const LARGE: &[i64] = &[i64::MAX, -i64::MAX, 1 << 62, 3_037_000_500, -(1 << 32)];

/// An expression without parts: a number, a variable of `scope` when
/// there is one, or the length of a short list.
fn leaf(rng: &mut Rng, scope: &[String]) -> Expr {
    match rng.below(16) {
        0..=6 => Expr::Lit(rng.below(200) as i64 - 100),
        7 => Expr::Lit(LARGE[rng.below(LARGE.len() as u64) as usize]),
        8..=13 if !scope.is_empty() => {
            Expr::Var(scope[rng.below(scope.len() as u64) as usize].clone())
        }
        8..=13 => Expr::Lit(0),
        _ => Expr::ListLength(rng.below(5) as u8),
    }
}

/// An expression that nests at most `depth` deep, over the variables
/// of `scope`.
fn expr(rng: &mut Rng, scope: &mut Vec<String>, depth: u32) -> Expr {
    if depth == 0 {
        return leaf(rng, scope);
    }
    let part = |rng: &mut Rng, scope: &mut Vec<String>| Box::new(expr(rng, scope, depth - 1));
    // `part` where `name` is in scope too.
    let within = |rng: &mut Rng, scope: &mut Vec<String>, name: &str| {
        scope.push(name.to_string());
        let body = Box::new(expr(rng, scope, depth - 1));
        scope.pop();
        body
    };
    match rng.below(15) {
        0..=1 => leaf(rng, scope),
        2..=4 => {
            let op = ['+', '-', '*'][rng.below(3) as usize];
            Expr::Binary(part(rng, scope), op, part(rng, scope))
        }
        5..=6 => Expr::IfPositive {
            cond: part(rng, scope),
            then: part(rng, scope),
            otherwise: part(rng, scope),
        },
        7..=8 => {
            let name = name(rng);
            Expr::Let {
                bound: part(rng, scope),
                body: within(rng, scope, &name),
                name,
            }
        }
        9..=10 => Expr::EitherZero {
            a: part(rng, scope),
            b: part(rng, scope),
            hit: part(rng, scope),
            miss: part(rng, scope),
        },
        11..=12 => {
            let param = name(rng);
            Expr::Call {
                arg: part(rng, scope),
                body: within(rng, scope, &param),
                param,
            }
        }
        13 => Expr::Pipe {
            seed: part(rng, scope),
            bias: within(rng, scope, "v"),
        },
        _ => Expr::TupleLet {
            a: part(rng, scope),
            b: part(rng, scope),
        },
    }
}

/// The expression of the program `number` of `seed`.
fn generate(seed: u64, number: usize) -> Expr {
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ number as u64);
    let depth = rng.below(u64::from(MAX_DEPTH)) as u32;
    expr(&mut rng, &mut Vec::new(), depth)
}

// ── The inputs ──────────────────────────────────────────────────────

/// The input whose program is `expr`: it must end as the reference
/// evaluator says.
fn input(name: String, expr: &Expr) -> Input {
    let end = eval(expr, &mut Vec::new()).map(Value::Int);
    Input {
        name,
        source: Source::Memory(vec![("main.silt".to_string(), program(expr))]),
        real_time: false,
        expect: Expect {
            end: Some(end),
            ..Expect::default()
        },
    }
}

/// The oracle's verdict on `input`, within the sweep's step budget.
fn examine_within(input: &Input) -> Verdict {
    examine(input, steps())
}

/// The expressions `expr` can be cut down to in one step: one of its
/// parts in its place (where the part needs no variable `expr` binds),
/// or a part made smaller.
fn smaller(expr: &Expr) -> Vec<Expr> {
    // The variants of `expr` with the part `part` made smaller, each
    // put back by `rebuild`.
    fn with(part: &Expr, rebuild: impl Fn(Box<Expr>) -> Expr) -> Vec<Expr> {
        let mut out = vec![rebuild(Box::new(Expr::Lit(1)))];
        out.extend(smaller(part).into_iter().map(|p| rebuild(Box::new(p))));
        out.retain(|candidate| *candidate != rebuild(Box::new(part.clone())));
        out
    }
    let own = |e: &Expr| Box::new(e.clone());
    match expr {
        Expr::Lit(0 | 1) => Vec::new(),
        Expr::Lit(_) | Expr::Var(_) | Expr::ListLength(_) => vec![Expr::Lit(1)],
        Expr::Binary(a, op, b) => {
            let mut out = vec![(**a).clone(), (**b).clone()];
            out.extend(with(a, |a| Expr::Binary(a, *op, own(b))));
            out.extend(with(b, |b| Expr::Binary(own(a), *op, b)));
            out
        }
        Expr::IfPositive {
            cond,
            then,
            otherwise,
        } => {
            let rebuild = |cond, then, otherwise| Expr::IfPositive {
                cond,
                then,
                otherwise,
            };
            let mut out = vec![(**cond).clone(), (**then).clone(), (**otherwise).clone()];
            out.extend(with(cond, |c| rebuild(c, own(then), own(otherwise))));
            out.extend(with(then, |t| rebuild(own(cond), t, own(otherwise))));
            out.extend(with(otherwise, |o| rebuild(own(cond), own(then), o)));
            out
        }
        Expr::Let { name, bound, body } => {
            let rebuild = |bound, body| Expr::Let {
                name: name.clone(),
                bound,
                body,
            };
            let mut out = vec![(**bound).clone()];
            out.extend(with(bound, |b| rebuild(b, own(body))));
            out.extend(with(body, |b| rebuild(own(bound), b)));
            out
        }
        Expr::EitherZero { a, b, hit, miss } => {
            let rebuild = |a, b, hit, miss| Expr::EitherZero { a, b, hit, miss };
            let mut out = vec![
                (**a).clone(),
                (**b).clone(),
                (**hit).clone(),
                (**miss).clone(),
            ];
            out.extend(with(a, |x| rebuild(x, own(b), own(hit), own(miss))));
            out.extend(with(b, |x| rebuild(own(a), x, own(hit), own(miss))));
            out.extend(with(hit, |x| rebuild(own(a), own(b), x, own(miss))));
            out.extend(with(miss, |x| rebuild(own(a), own(b), own(hit), x)));
            out
        }
        Expr::Call { param, arg, body } => {
            let rebuild = |arg, body| Expr::Call {
                param: param.clone(),
                arg,
                body,
            };
            let mut out = vec![(**arg).clone()];
            out.extend(with(arg, |a| rebuild(a, own(body))));
            out.extend(with(body, |b| rebuild(own(arg), b)));
            out
        }
        Expr::Pipe { seed, bias } => {
            let mut out = vec![(**seed).clone()];
            out.extend(with(seed, |s| Expr::Pipe {
                seed: s,
                bias: own(bias),
            }));
            out.extend(with(bias, |b| Expr::Pipe {
                seed: own(seed),
                bias: b,
            }));
            out
        }
        Expr::TupleLet { a, b } => {
            let mut out = vec![(**a).clone(), (**b).clone()];
            out.extend(with(a, |a| Expr::TupleLet { a, b: own(b) }));
            out.extend(with(b, |b| Expr::TupleLet { a: own(a), b }));
            out
        }
    }
}

/// The smallest expression, reached from `expr` by [`smaller`] steps,
/// for which `fails` still holds.
fn minimise(mut expr: Expr, fails: impl Fn(&Expr) -> bool) -> Expr {
    while let Some(next) = smaller(&expr)
        .into_iter()
        .find(|candidate| fails(candidate))
    {
        expr = next;
    }
    expr
}

/// The smallest expression reached from `expr` that still has the
/// finding `found`, with that finding as the smallest shows it:
/// `finding_of` gives the finding of an expression. A candidate counts
/// only when its finding is the SAME one, of the kind and about the
/// same thing ([`Finding::what`]): an expression can have two defects,
/// and a shrinker that takes any finding of the kind ends at the other
/// one.
fn smallest_with(
    expr: Expr,
    found: &Finding,
    finding_of: impl Fn(&Expr) -> Option<Finding>,
) -> (Expr, Finding) {
    let same = |candidate: &Expr| {
        finding_of(candidate).filter(|f| f.kind == found.kind && f.what == found.what)
    };
    let smallest = minimise(expr, |candidate| same(candidate).is_some());
    let shown = same(&smallest).unwrap_or_else(|| found.clone());
    (smallest, shown)
}

#[test]
fn generated_programs_end_as_the_reference_evaluator_says() {
    let seed = match std::env::var("SILT_ORACLE_SEED") {
        Ok(seed) => seed.parse().expect("SILT_ORACLE_SEED is a number"),
        Err(_) => SEED,
    };
    let count = if full() { PROGRAMS_FULL } else { PROGRAMS };
    let exprs: Vec<Expr> = (0..count).map(|number| generate(seed, number)).collect();
    let all: Vec<Input> = exprs
        .iter()
        .enumerate()
        .map(|(number, expr)| input(format!("generated/{seed}/{number}"), expr))
        .collect();
    check_listed(&all, |name| name.starts_with("generated/"));
    let skips = skips();
    let inputs = chosen(all);
    let mut verdicts = run(&inputs, &skips);

    let mut overflows = 0;
    for (input, (verdict, _)) in inputs.iter().zip(&mut verdicts) {
        // Every program of the subset checks clean, names `list` alone
        // and ends within a few hundred steps: one that is not run, or
        // not to its end, is a defect of the generator.
        assert!(
            !matches!(verdict, Verdict::NotRun(_) | Verdict::Cut(_)),
            "{} was not run ({verdict:?}):\n{}",
            input.name,
            source_of(input)
        );
        overflows += usize::from(matches!(input.expect.end, Some(Err(_))));
        // A finding is shown with the smallest program that has it.
        if let Verdict::Finding(finding) = verdict {
            let number: usize = input.name.rsplit('/').next().unwrap().parse().unwrap();
            let (smallest, same) =
                smallest_with(
                    exprs[number].clone(),
                    finding,
                    |candidate| match examine_within(&self::input(input.name.clone(), candidate)) {
                        Verdict::Finding(finding) => Some(finding),
                        _ => None,
                    },
                );
            let end = match eval(&smallest, &mut Vec::new()) {
                Ok(n) => format!("main returns {n}"),
                Err(message) => format!("a runtime error: {message}"),
            };
            *finding = same;
            finding.detail.push_str(&format!(
                "\nthe smallest program with the finding (the reference evaluator: {end}):\n{}",
                program(&smallest)
            ));
        }
    }
    eprintln!(
        "generated programs: {} of {} end in an integer overflow",
        overflows,
        inputs.len()
    );
    conclude("generated programs", &inputs, &verdicts, &skips);
}

/// The text of the program of a generated input.
fn source_of(input: &Input) -> &str {
    match &input.source {
        Source::Memory(files) => &files[0].1,
        _ => unreachable!("a generated program is in memory"),
    }
}

/// The reference evaluator and the renderer on programs whose value is
/// plain to see.
#[test]
fn the_reference_evaluator_follows_scopes_and_stops_at_an_overflow() {
    let lit = |n| Box::new(Expr::Lit(n));
    let var = |name: &str| Box::new(Expr::Var(name.to_string()));
    let value = |expr: &Expr| eval(expr, &mut Vec::new());

    // `{ let x = 2  { x -> x * 10 }(x + 1) }`: the parameter shadows.
    let shadowed = Expr::Let {
        name: "x".into(),
        bound: lit(2),
        body: Box::new(Expr::Call {
            param: "x".into(),
            arg: Box::new(Expr::Binary(var("x"), '+', lit(1))),
            body: Box::new(Expr::Binary(var("x"), '*', lit(10))),
        }),
    };
    assert_eq!(value(&shadowed), Ok(30));

    // `{ let v = 5  10 |> { v -> v + v } }`: the bias reads the pipe's `v`.
    let piped = Expr::Let {
        name: "v".into(),
        bound: lit(5),
        body: Box::new(Expr::Pipe {
            seed: lit(10),
            bias: var("v"),
        }),
    };
    assert_eq!(value(&piped), Ok(20));

    // The branch that is not taken is not evaluated.
    let overflow = || Box::new(Expr::Binary(lit(i64::MAX), '*', lit(-2)));
    let untaken = Expr::IfPositive {
        cond: lit(0),
        then: overflow(),
        otherwise: lit(7),
    };
    assert_eq!(value(&untaken), Ok(7));
    let taken = Expr::EitherZero {
        a: lit(3),
        b: Box::new(Expr::ListLength(0)),
        hit: overflow(),
        miss: lit(7),
    };
    assert_eq!(
        value(&taken),
        Err("integer overflow: 9223372036854775807 * -2".to_string())
    );

    // The VM agrees on each, and the text is a program that checks.
    for expr in [shadowed, piped, untaken, taken] {
        let verdict = examine_within(&input("selfcheck".to_string(), &expr));
        assert!(
            matches!(verdict, Verdict::Passed(_)),
            "{verdict:?}\n{}",
            program(&expr)
        );
    }
}

/// A program that ends otherwise than its expectation says is a
/// finding, and is cut down to the part that matters.
#[test]
fn a_program_that_ends_otherwise_than_expected_is_a_finding_and_is_minimised() {
    let lit = |n| Box::new(Expr::Lit(n));
    let expr = Expr::Binary(
        Box::new(Expr::TupleLet {
            a: lit(4),
            b: lit(5),
        }),
        '*',
        Box::new(Expr::Binary(lit(6), '-', Box::new(Expr::ListLength(2)))),
    );
    let mut wrong = input("selfcheck".to_string(), &expr);
    wrong.expect.end = Some(Ok(Value::Int(35)));
    match examine_within(&wrong) {
        Verdict::Finding(finding) => {
            assert_eq!(finding.kind.name(), "expectation");
            assert!(
                finding.detail.contains("main returned 36"),
                "{}",
                finding.detail
            );
        }
        other => panic!("{other:?}"),
    }
    // "Fails" while the expression still multiplies.
    let multiplies = |expr: &Expr| matches!(expr, Expr::Binary(_, '*', _));
    assert_eq!(
        minimise(expr, multiplies),
        Expr::Binary(lit(1), '*', lit(1))
    );
}

/// An expression with two defects of one kind is cut down to the one
/// that was found, not to whichever is left: here every product "is
/// wrong in its value" and every `list.length` "is wrong in its
/// stdout", both findings of the kind `expectation`.
#[test]
fn the_smallest_program_has_the_same_finding_not_another_of_its_kind() {
    use crate::oracle::Kind;

    fn has(expr: &Expr, wanted: &dyn Fn(&Expr) -> bool) -> bool {
        wanted(expr) || smaller(expr).iter().any(|part| has(part, wanted))
    }
    let planted = |what: &str| Finding {
        kind: Kind::Expectation,
        what: what.to_string(),
        detail: format!("planted: {what}"),
    };
    let finding_of = |expr: &Expr| {
        if has(expr, &|e| matches!(e, Expr::Binary(_, '*', _))) {
            Some(planted("main's value"))
        } else if has(expr, &|e| matches!(e, Expr::ListLength(_))) {
            Some(planted("stdout"))
        } else {
            None
        }
    };
    let lit = |n| Box::new(Expr::Lit(n));
    let both = Expr::Binary(
        Box::new(Expr::ListLength(3)),
        '+',
        Box::new(Expr::Binary(lit(6), '*', Box::new(Expr::ListLength(2)))),
    );
    let found = finding_of(&both).unwrap();
    assert_eq!(found.what, "main's value");
    let (smallest, shown) = smallest_with(both.clone(), &found, finding_of);
    assert_eq!(smallest, Expr::Binary(lit(1), '*', lit(1)));
    assert_eq!(shown.what, "main's value");
    // A shrinker that takes any finding of the kind takes the first
    // part offered, the list's length, and ends at the other defect.
    let any = minimise(both, |candidate| finding_of(candidate).is_some());
    assert_eq!(any, Expr::ListLength(3));
}
