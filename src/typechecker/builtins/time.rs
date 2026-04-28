//! Type signatures for the `time` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.
//!
//! Effect-set classification (Phase C of the effect-rows proposal):
//!   - `time.now` / `time.today` / `time.sleep` read or block on the
//!     wall clock — `!{io, time}`.
//!   - Everything else is pure date / duration arithmetic that takes
//!     a value-typed `Instant` / `DateTime` / `Date` and computes a
//!     result without consulting the OS — `!{}`. The runtime impls
//!     in `src/builtins/data.rs` confirm this: only `now` / `today`
//!     / `sleep` reach for `vm.epoch_ms()` or `std::thread::sleep`.

use super::super::*;
use super::docs::attach_module_docs;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // ── Time module type definitions ──────────────────────────────

    let instant_ty =
        super::record_with_fields(checker, "Instant", vec![(intern("epoch_ns"), Type::Int)]);
    let date_ty = super::record_with_fields(
        checker,
        "Date",
        vec![
            (intern("year"), Type::Int),
            (intern("month"), Type::Int),
            (intern("day"), Type::Int),
        ],
    );
    let time_of_day_ty = super::record_with_fields(
        checker,
        "Time",
        vec![
            (intern("hour"), Type::Int),
            (intern("minute"), Type::Int),
            (intern("second"), Type::Int),
            (intern("ns"), Type::Int),
        ],
    );
    let datetime_ty = super::record_with_fields(
        checker,
        "DateTime",
        vec![
            (intern("date"), date_ty.clone()),
            (intern("time"), time_of_day_ty.clone()),
        ],
    );
    let duration_ty =
        super::record_with_fields(checker, "Duration", vec![(intern("ns"), Type::Int)]);
    let weekday_ty = Type::Generic(intern("Weekday"), vec![]);

    // Register Weekday enum
    checker.enums.insert(
        intern("Weekday"),
        EnumInfo {
            params: vec![],
            param_var_ids: vec![],
            variants: vec![
                VariantInfo {
                    name: intern("Monday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Tuesday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Wednesday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Thursday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Friday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Saturday"),
                    field_types: vec![],
                },
                VariantInfo {
                    name: intern("Sunday"),
                    field_types: vec![],
                },
            ],
            defined_in: super::super::TypeChecker::builtin_pkg(),
        },
    );
    let weekday_variants = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    for day in weekday_variants {
        checker
            .variant_to_enum
            .insert(intern(day), intern("Weekday"));
        env.define(intern(day), Scheme::pure_mono(weekday_ty.clone()));
    }
    // Register declaration-order ordinals so `cmp_gen(Monday, Friday)`
    // and `Monday < Friday` both honour the same ordering used by every
    // other enum. Replaces the hand-rolled `weekday_ordinal` table that
    // lived in `value.rs` before this change.
    crate::value::register_variant_decl_order(weekday_variants);

    // ── Register Display (and other builtin traits) for time types ──
    // Shares `register_auto_derived_impls_for` with the primitive-type
    // init path so a derive-policy change flows to every site.
    super::super::register_auto_derived_impls_for(
        checker,
        &["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"],
        super::super::BUILTIN_AUTO_DERIVED_TRAIT_NAMES,
    );

    // ── Function signatures ──────────────────────────────────────

    // time.now: () -> Instant — reads the system clock.
    env.define(
        intern("time.now"),
        Scheme::io_time_mono(Type::Fun(vec![], Box::new(instant_ty.clone()))),
    );

    // time.today: () -> Date — reads the system clock for the current
    // local-zone date.
    env.define(
        intern("time.today"),
        Scheme::io_time_mono(Type::Fun(vec![], Box::new(date_ty.clone()))),
    );

    // time.date: (Int, Int, Int) -> Result(Date, String) — pure
    // calendar-date constructor.
    env.define(
        intern("time.date"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Generic(
                intern("Result"),
                vec![date_ty.clone(), Type::Generic(intern("TimeError"), vec![])],
            )),
        )),
    );

    // time.time: (Int, Int, Int) -> Result(Time, String) — pure
    // time-of-day constructor.
    env.define(
        intern("time.time"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Generic(
                intern("Result"),
                vec![
                    time_of_day_ty.clone(),
                    Type::Generic(intern("TimeError"), vec![]),
                ],
            )),
        )),
    );

    // time.datetime: (Date, Time) -> DateTime — pure constructor.
    env.define(
        intern("time.datetime"),
        Scheme::pure_mono(Type::Fun(
            vec![date_ty.clone(), time_of_day_ty.clone()],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.to_datetime: (Instant, Int) -> DateTime — pure conversion
    // on a passed-in Instant + offset.
    env.define(
        intern("time.to_datetime"),
        Scheme::pure_mono(Type::Fun(
            vec![instant_ty.clone(), Type::Int],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.to_instant: (DateTime, Int) -> Instant — pure conversion.
    env.define(
        intern("time.to_instant"),
        Scheme::pure_mono(Type::Fun(
            vec![datetime_ty.clone(), Type::Int],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.to_utc: (Instant) -> DateTime — pure conversion (the
    // Instant was acquired earlier; no clock read here).
    env.define(
        intern("time.to_utc"),
        Scheme::pure_mono(Type::Fun(
            vec![instant_ty.clone()],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.from_utc: (DateTime) -> Instant — pure conversion.
    env.define(
        intern("time.from_utc"),
        Scheme::pure_mono(Type::Fun(
            vec![datetime_ty.clone()],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.format: (DateTime, String) -> String — pure formatter.
    env.define(
        intern("time.format"),
        Scheme::pure_mono(Type::Fun(
            vec![datetime_ty.clone(), Type::String],
            Box::new(Type::String),
        )),
    );

    // time.format_date: (Date, String) -> String — pure formatter.
    env.define(
        intern("time.format_date"),
        Scheme::pure_mono(Type::Fun(
            vec![date_ty.clone(), Type::String],
            Box::new(Type::String),
        )),
    );

    // time.parse: (String, String) -> Result(DateTime, String) — pure
    // parser.
    env.define(
        intern("time.parse"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![
                    datetime_ty.clone(),
                    Type::Generic(intern("TimeError"), vec![]),
                ],
            )),
        )),
    );

    // time.parse_date: (String, String) -> Result(Date, String) — pure
    // parser.
    env.define(
        intern("time.parse_date"),
        Scheme::pure_mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![date_ty.clone(), Type::Generic(intern("TimeError"), vec![])],
            )),
        )),
    );

    // time.add_days: (Date, Int) -> Date — pure calendar arithmetic.
    env.define(
        intern("time.add_days"),
        Scheme::pure_mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )),
    );

    // time.add_months: (Date, Int) -> Date — pure calendar arithmetic.
    env.define(
        intern("time.add_months"),
        Scheme::pure_mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )),
    );

    // time.add: (Instant, Duration) -> Instant — pure arithmetic.
    env.define(
        intern("time.add"),
        Scheme::pure_mono(Type::Fun(
            vec![instant_ty.clone(), duration_ty.clone()],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.since: (Instant, Instant) -> Duration — pure subtraction.
    env.define(
        intern("time.since"),
        Scheme::pure_mono(Type::Fun(
            vec![instant_ty.clone(), instant_ty.clone()],
            Box::new(duration_ty.clone()),
        )),
    );

    // Duration constructors are pure.
    env.define(
        intern("time.hours"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );
    env.define(
        intern("time.minutes"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );
    env.define(
        intern("time.seconds"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );
    env.define(
        intern("time.ms"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );
    env.define(
        intern("time.micros"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );
    env.define(
        intern("time.nanos"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.weekday: (Date) -> Weekday — pure.
    env.define(
        intern("time.weekday"),
        Scheme::pure_mono(Type::Fun(vec![date_ty.clone()], Box::new(weekday_ty))),
    );

    // time.days_between: (Date, Date) -> Int — pure.
    env.define(
        intern("time.days_between"),
        Scheme::pure_mono(Type::Fun(
            vec![date_ty.clone(), date_ty.clone()],
            Box::new(Type::Int),
        )),
    );

    // time.days_in_month: (Int, Int) -> Int — pure.
    env.define(
        intern("time.days_in_month"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
    );

    // time.is_leap_year: (Int) -> Bool — pure.
    env.define(
        intern("time.is_leap_year"),
        Scheme::pure_mono(Type::Fun(vec![Type::Int], Box::new(Type::Bool))),
    );

    // time.sleep: (Duration) -> Unit — blocks on the wall clock.
    env.define(
        intern("time.sleep"),
        Scheme::io_time_mono(Type::Fun(vec![duration_ty], Box::new(Type::Unit))),
    );

    attach_module_docs(env, super::docs::TIME_MD);
}
