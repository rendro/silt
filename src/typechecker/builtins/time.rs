//! Type signatures for the `time` builtin module.
//!
//! Extracted from the former monolithic `src/typechecker/builtins.rs`.

use super::super::*;

pub(super) fn register(checker: &mut TypeChecker, env: &mut TypeEnv) {
    // ── Time module type definitions ──────────────────────────────

    let instant_ty = Type::Record(intern("Instant"), vec![(intern("epoch_ns"), Type::Int)]);
    let date_ty = Type::Record(
        intern("Date"),
        vec![
            (intern("year"), Type::Int),
            (intern("month"), Type::Int),
            (intern("day"), Type::Int),
        ],
    );
    let time_of_day_ty = Type::Record(
        intern("Time"),
        vec![
            (intern("hour"), Type::Int),
            (intern("minute"), Type::Int),
            (intern("second"), Type::Int),
            (intern("ns"), Type::Int),
        ],
    );
    let datetime_ty = Type::Record(
        intern("DateTime"),
        vec![
            (intern("date"), date_ty.clone()),
            (intern("time"), time_of_day_ty.clone()),
        ],
    );
    let duration_ty = Type::Record(intern("Duration"), vec![(intern("ns"), Type::Int)]);
    let weekday_ty = Type::Generic(intern("Weekday"), vec![]);

    // Register record types so field access type-checks
    checker.records.insert(
        intern("Instant"),
        RecordInfo {
            _name: intern("Instant"),
            _params: vec![],
            fields: vec![(intern("epoch_ns"), Type::Int)],
        },
    );
    checker.records.insert(
        intern("Date"),
        RecordInfo {
            _name: intern("Date"),
            _params: vec![],
            fields: vec![
                (intern("year"), Type::Int),
                (intern("month"), Type::Int),
                (intern("day"), Type::Int),
            ],
        },
    );
    checker.records.insert(
        intern("Time"),
        RecordInfo {
            _name: intern("Time"),
            _params: vec![],
            fields: vec![
                (intern("hour"), Type::Int),
                (intern("minute"), Type::Int),
                (intern("second"), Type::Int),
                (intern("ns"), Type::Int),
            ],
        },
    );
    checker.records.insert(
        intern("DateTime"),
        RecordInfo {
            _name: intern("DateTime"),
            _params: vec![],
            fields: vec![
                (intern("date"), date_ty.clone()),
                (intern("time"), time_of_day_ty.clone()),
            ],
        },
    );
    checker.records.insert(
        intern("Duration"),
        RecordInfo {
            _name: intern("Duration"),
            _params: vec![],
            fields: vec![(intern("ns"), Type::Int)],
        },
    );

    // Register Weekday enum
    checker.enums.insert(
        intern("Weekday"),
        EnumInfo {
            _name: intern("Weekday"),
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
        },
    );
    for day in [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ] {
        checker
            .variant_to_enum
            .insert(intern(day), intern("Weekday"));
        env.define(intern(day), Scheme::mono(weekday_ty.clone()));
    }

    // ── Register Display (and other builtin traits) for time types ──
    // Shares `register_auto_derived_impls_for` with the primitive-type
    // init path so a derive-policy change flows to every site.
    super::super::register_auto_derived_impls_for(
        checker,
        &["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"],
        super::super::BUILTIN_AUTO_DERIVED_TRAIT_NAMES,
    );

    // ── Function signatures ──────────────────────────────────────

    // time.now: () -> Instant
    env.define(
        intern("time.now"),
        Scheme::mono(Type::Fun(vec![], Box::new(instant_ty.clone()))),
    );

    // time.today: () -> Date
    env.define(
        intern("time.today"),
        Scheme::mono(Type::Fun(vec![], Box::new(date_ty.clone()))),
    );

    // time.date: (Int, Int, Int) -> Result(Date, String)
    env.define(
        intern("time.date"),
        Scheme::mono(Type::Fun(
            vec![Type::Int, Type::Int, Type::Int],
            Box::new(Type::Generic(
                intern("Result"),
                vec![date_ty.clone(), Type::Generic(intern("TimeError"), vec![])],
            )),
        )),
    );

    // time.time: (Int, Int, Int) -> Result(Time, String)
    env.define(
        intern("time.time"),
        Scheme::mono(Type::Fun(
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

    // time.datetime: (Date, Time) -> DateTime
    env.define(
        intern("time.datetime"),
        Scheme::mono(Type::Fun(
            vec![date_ty.clone(), time_of_day_ty.clone()],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.to_datetime: (Instant, Int) -> DateTime
    env.define(
        intern("time.to_datetime"),
        Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), Type::Int],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.to_instant: (DateTime, Int) -> Instant
    env.define(
        intern("time.to_instant"),
        Scheme::mono(Type::Fun(
            vec![datetime_ty.clone(), Type::Int],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.to_utc: (Instant) -> DateTime
    env.define(
        intern("time.to_utc"),
        Scheme::mono(Type::Fun(
            vec![instant_ty.clone()],
            Box::new(datetime_ty.clone()),
        )),
    );

    // time.from_utc: (DateTime) -> Instant
    env.define(
        intern("time.from_utc"),
        Scheme::mono(Type::Fun(
            vec![datetime_ty.clone()],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.format: (DateTime, String) -> String
    env.define(
        intern("time.format"),
        Scheme::mono(Type::Fun(
            vec![datetime_ty.clone(), Type::String],
            Box::new(Type::String),
        )),
    );

    // time.format_date: (Date, String) -> String
    env.define(
        intern("time.format_date"),
        Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::String],
            Box::new(Type::String),
        )),
    );

    // time.parse: (String, String) -> Result(DateTime, String)
    env.define(
        intern("time.parse"),
        Scheme::mono(Type::Fun(
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

    // time.parse_date: (String, String) -> Result(Date, String)
    env.define(
        intern("time.parse_date"),
        Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![date_ty.clone(), Type::Generic(intern("TimeError"), vec![])],
            )),
        )),
    );

    // time.add_days: (Date, Int) -> Date
    env.define(
        intern("time.add_days"),
        Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )),
    );

    // time.add_months: (Date, Int) -> Date
    env.define(
        intern("time.add_months"),
        Scheme::mono(Type::Fun(
            vec![date_ty.clone(), Type::Int],
            Box::new(date_ty.clone()),
        )),
    );

    // time.add: (Instant, Duration) -> Instant
    env.define(
        intern("time.add"),
        Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), duration_ty.clone()],
            Box::new(instant_ty.clone()),
        )),
    );

    // time.since: (Instant, Instant) -> Duration
    env.define(
        intern("time.since"),
        Scheme::mono(Type::Fun(
            vec![instant_ty.clone(), instant_ty.clone()],
            Box::new(duration_ty.clone()),
        )),
    );

    // time.hours: (Int) -> Duration
    env.define(
        intern("time.hours"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.minutes: (Int) -> Duration
    env.define(
        intern("time.minutes"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.seconds: (Int) -> Duration
    env.define(
        intern("time.seconds"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.ms: (Int) -> Duration
    env.define(
        intern("time.ms"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.micros: (Int) -> Duration
    env.define(
        intern("time.micros"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.nanos: (Int) -> Duration
    env.define(
        intern("time.nanos"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
    );

    // time.weekday: (Date) -> Weekday
    env.define(
        intern("time.weekday"),
        Scheme::mono(Type::Fun(vec![date_ty.clone()], Box::new(weekday_ty))),
    );

    // time.days_between: (Date, Date) -> Int
    env.define(
        intern("time.days_between"),
        Scheme::mono(Type::Fun(
            vec![date_ty.clone(), date_ty.clone()],
            Box::new(Type::Int),
        )),
    );

    // time.days_in_month: (Int, Int) -> Int
    env.define(
        intern("time.days_in_month"),
        Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
    );

    // time.is_leap_year: (Int) -> Bool
    env.define(
        intern("time.is_leap_year"),
        Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Bool))),
    );

    // time.sleep: (Duration) -> Unit
    env.define(
        intern("time.sleep"),
        Scheme::mono(Type::Fun(vec![duration_ty], Box::new(Type::Unit))),
    );
}
