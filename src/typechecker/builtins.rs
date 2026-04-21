//! Builtin type registrations for the standard library modules.
//!
//! The `register_builtins` method populates the type environment with
//! type signatures for the language core (constructors, enums, task,
//! regex, json, primitive descriptors) and dispatches per-module
//! registrations to the corresponding submodule.
//!
//! Per-module type signatures live in submodules under
//! `src/typechecker/builtins/`, one file per stdlib module. Each
//! submodule exposes a `register(checker, env)` free function; the
//! thin `register_<name>_builtins` wrappers below keep the per-module
//! names visible to the `docs_round26_tests.rs` coverage walker.

use super::*;

mod bytes;
mod channel;
mod crypto;
mod encoding;
mod env;
mod errors;
mod float;
mod fs;
mod http;
mod int;
mod io;
mod list;
mod map;
mod math;
mod option;
mod postgres;
mod result;
mod set;
mod stream;
mod string;
#[cfg(feature = "tcp")]
mod tcp;
mod test;
mod time;
mod toml;
mod uuid;

impl TypeChecker {
    pub(super) fn register_builtins(&mut self, env: &mut TypeEnv) {
        // ── print / println: (a) -> () where a: Display ────────────────
        // The runtime uses Display for formatting, so the argument must
        // implement Display.
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("print"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                    constraints: vec![(av, intern("Display"))],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("println"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                    constraints: vec![(av, intern("Display"))],
                },
            );
        }

        // ── panic: a -> Never where a: Display (never returns) ─────────
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("panic"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::Never)),
                    constraints: vec![(av, intern("Display"))],
                },
            );
        }

        // ── Variant constructors ───────────────────────────────────────

        // Ok(a) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("Ok"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic(intern("Result"), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }
        // Err(e) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("Err"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![e.clone()],
                        Box::new(Type::Generic(intern("Result"), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }
        // Some(a) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Some"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic(intern("Option"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        // None : Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("None"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("Option"), vec![a]),
                    constraints: vec![],
                },
            );
        }

        // ── Builtin enum info for Option and Result ────────────────────

        // Use fresh type variables (allocated via fresh_tv) for builtin enum
        // param_var_ids, just like register_type_decl does for user-defined enums.
        // This avoids overlap with real TyVar IDs from fresh_var().

        // Option(a): Some(a) | None
        {
            let (opt_a, opt_av) = self.fresh_tv();
            self.enums.insert(
                intern("Option"),
                EnumInfo {
                    _name: intern("Option"),
                    params: vec![intern("a")],
                    param_var_ids: vec![opt_av],
                    variants: vec![
                        VariantInfo {
                            name: intern("Some"),
                            field_types: vec![opt_a],
                        },
                        VariantInfo {
                            name: intern("None"),
                            field_types: vec![],
                        },
                    ],
                },
            );
        }
        self.variant_to_enum
            .insert(intern("Some"), intern("Option"));
        self.variant_to_enum
            .insert(intern("None"), intern("Option"));

        // Result(a, e): Ok(a) | Err(e)
        {
            let (res_a, res_av) = self.fresh_tv();
            let (res_e, res_ev) = self.fresh_tv();
            self.enums.insert(
                intern("Result"),
                EnumInfo {
                    _name: intern("Result"),
                    params: vec![intern("a"), intern("e")],
                    param_var_ids: vec![res_av, res_ev],
                    variants: vec![
                        VariantInfo {
                            name: intern("Ok"),
                            field_types: vec![res_a],
                        },
                        VariantInfo {
                            name: intern("Err"),
                            field_types: vec![res_e],
                        },
                    ],
                },
            );
        }
        self.variant_to_enum.insert(intern("Ok"), intern("Result"));
        self.variant_to_enum.insert(intern("Err"), intern("Result"));

        // Step enum: Stop(a) / Continue(a) — for list.fold_until
        {
            let (step_a, step_av) = self.fresh_tv();
            self.enums.insert(
                intern("Step"),
                EnumInfo {
                    _name: intern("Step"),
                    params: vec![intern("a")],
                    param_var_ids: vec![step_av],
                    variants: vec![
                        VariantInfo {
                            name: intern("Stop"),
                            field_types: vec![step_a.clone()],
                        },
                        VariantInfo {
                            name: intern("Continue"),
                            field_types: vec![step_a],
                        },
                    ],
                },
            );
        }
        self.variant_to_enum.insert(intern("Stop"), intern("Step"));
        self.variant_to_enum
            .insert(intern("Continue"), intern("Step"));
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Stop"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic(intern("Step"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Continue"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic(intern("Step"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // ChannelResult enum: Message(a) / Closed — for channel.receive
        {
            let (cr_a, cr_av) = self.fresh_tv();
            self.enums.insert(
                intern("ChannelResult"),
                EnumInfo {
                    _name: intern("ChannelResult"),
                    params: vec![intern("a")],
                    param_var_ids: vec![cr_av],
                    variants: vec![
                        VariantInfo {
                            name: intern("Message"),
                            field_types: vec![cr_a],
                        },
                        VariantInfo {
                            name: intern("Closed"),
                            field_types: vec![],
                        },
                        VariantInfo {
                            name: intern("Sent"),
                            field_types: vec![],
                        },
                        VariantInfo {
                            name: intern("Empty"),
                            field_types: vec![],
                        },
                    ],
                },
            );
        }
        self.variant_to_enum
            .insert(intern("Message"), intern("ChannelResult"));
        self.variant_to_enum
            .insert(intern("Closed"), intern("ChannelResult"));
        // Also register Empty and Sent as standalones
        self.variant_to_enum
            .insert(intern("Empty"), intern("ChannelResult"));
        self.variant_to_enum
            .insert(intern("Sent"), intern("ChannelResult"));
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Message"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic(intern("ChannelResult"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Closed"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("ChannelResult"), vec![a]),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Empty"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("ChannelResult"), vec![a]),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Sent"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("ChannelResult"), vec![a]),
                    constraints: vec![],
                },
            );
        }

        // ── task module ────────────────────────────────────────────────

        // task.spawn: (() -> a) -> Handle(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("task.spawn"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Fun(vec![], Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("Handle"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // task.join: Handle(a) -> a
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("task.join"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Handle"), vec![a.clone()])],
                        Box::new(a),
                    ),
                    constraints: vec![],
                },
            );
        }

        // task.cancel: Handle(a) -> Unit
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("task.cancel"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Handle"), vec![a])],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // task.deadline: (Duration, () -> a) -> a
        // Runs the callback with a scoped I/O deadline. Returns whatever
        // the callback returns. If an I/O operation inside the callback
        // exceeds the deadline, it returns `Err(String)` with a
        // "I/O timeout (task.deadline exceeded)" message — silt code
        // already handles that via the normal Result matching.
        {
            let (a, av) = self.fresh_tv();
            let duration_ty = Type::Record(intern("Duration"), vec![(intern("ns"), Type::Int)]);
            env.define(
                intern("task.deadline"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![duration_ty, Type::Fun(vec![], Box::new(a.clone()))],
                        Box::new(a),
                    ),
                    constraints: vec![],
                },
            );
        }

        // task.spawn_until: (Duration, () -> a) -> Handle(a)
        // Spawns a task that runs with a scoped wall-clock deadline.
        // Equivalent to `task.spawn(fn() { task.deadline(dur, fn) })`
        // but with one less closure wrapper.
        {
            let (a, av) = self.fresh_tv();
            let duration_ty = Type::Record(intern("Duration"), vec![(intern("ns"), Type::Int)]);
            env.define(
                intern("task.spawn_until"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![duration_ty, Type::Fun(vec![], Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("Handle"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // ── regex module ────────────────────────────────────────────────

        // regex.is_match: (String, String) -> Bool
        env.define(
            intern("regex.is_match"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // regex.find: (String, String) -> Option(String)
        env.define(
            intern("regex.find"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(intern("Option"), vec![Type::String])),
            )),
        );

        // regex.find_all: (String, String) -> List(String)
        env.define(
            intern("regex.find_all"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // regex.split: (String, String) -> List(String)
        env.define(
            intern("regex.split"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // regex.replace: (String, String, String) -> String
        env.define(
            intern("regex.replace"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // regex.replace_all: (String, String, String) -> String
        env.define(
            intern("regex.replace_all"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // regex.replace_all_with: (String, String, (String) -> String) -> String
        env.define(
            intern("regex.replace_all_with"),
            Scheme::mono(Type::Fun(
                vec![
                    Type::String,
                    Type::String,
                    Type::Fun(vec![Type::String], Box::new(Type::String)),
                ],
                Box::new(Type::String),
            )),
        );

        // regex.captures: (String, String) -> Option(List(String))
        env.define(
            intern("regex.captures"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    intern("Option"),
                    vec![Type::List(Box::new(Type::String))],
                )),
            )),
        );

        // regex.captures_all: (String, String) -> List(List(String))
        env.define(
            intern("regex.captures_all"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::List(Box::new(Type::String))))),
            )),
        );

        // regex.captures_named: (String, String) -> Option(Map(String, String))
        // Returns a map of named-group name → matched substring for the
        // first match. `None` if the pattern has no named groups or the
        // regex does not match. Named groups that are present in the
        // pattern but did not participate in this match are omitted from
        // the map (rather than being mapped to `""`).
        env.define(
            intern("regex.captures_named"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    intern("Option"),
                    vec![Type::Map(Box::new(Type::String), Box::new(Type::String))],
                )),
            )),
        );

        // ── json module ─────────────────────────────────────────────────

        // json.parse: (String, type a) -> Result(a, String)
        // The `type a` parameter is lowered to a `TypeOf(a)` descriptor in
        // the function type; the carried type flows into the Result. Type
        // params come last so pipelines like
        //   body |> json.parse(Todo)
        // compose naturally (pipe inserts the piped value as first arg).
        {
            let (a, av) = self.fresh_tv();
            let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
            let result_ty = Type::Generic(intern("Result"), vec![a, Type::String]);
            env.define(
                intern("json.parse"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_list: (String, type a) -> Result(List(a), String)
        {
            let (a, av) = self.fresh_tv();
            let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
            let result_ty = Type::Generic(
                intern("Result"),
                vec![Type::List(Box::new(a)), Type::String],
            );
            env.define(
                intern("json.parse_list"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_map: (String, type v) -> Result(Map(String, v), String)
        {
            let (a, av) = self.fresh_tv();
            let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
            let result_ty = Type::Generic(
                intern("Result"),
                vec![Type::Map(Box::new(Type::String), Box::new(a)), Type::String],
            );
            env.define(
                intern("json.parse_map"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::String, descriptor_ty], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.stringify: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("json.stringify"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // json.pretty: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("json.pretty"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // ── Primitive type descriptors (for json.parse_map etc.) ──────
        // These carry the actual type as a TypeOf(T) descriptor so
        // json.parse can propagate it into the return type. They must
        // NOT be registered as the underlying type itself — otherwise
        // `Int * 2` would typecheck when `Int` is the bare descriptor
        // (T2 audit fix: the runtime represents the value as
        // `Value::PrimitiveDescriptor("Int")`, not `Value::Int(_)`).
        for name in &["Int", "Float", "String", "Bool"] {
            let inner = match *name {
                "Int" => Type::Int,
                "Float" => Type::Float,
                "String" => Type::String,
                "Bool" => Type::Bool,
                _ => unreachable!(),
            };
            env.define(
                intern(name),
                Scheme {
                    vars: vec![],
                    ty: Type::Generic(intern("TypeOf"), vec![inner]),
                    constraints: vec![],
                },
            );
        }

        // ── Builtin container descriptors ──────────────────────────────
        // Parallel to records/enums: `List`, `Map`, `Set`, `Channel` are
        // registered as polymorphic type descriptors so they can be
        // passed to `type a` parameters (`make(type t) where t: Empty`
        // called as `make(List)`). At runtime the compiler emits these
        // as `Value::TypeDescriptor(<name>)` globals. Method dispatch
        // via the descriptor routes to method_table[(<name>, method)]
        // exactly like user-defined parameterized types.
        {
            // `List` → forall a. TypeOf(List(a))
            let (a, av) = self.fresh_tv();
            env.define(
                intern("List"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("TypeOf"), vec![Type::List(Box::new(a))]),
                    constraints: vec![],
                },
            );
        }
        {
            // `Set` → forall a. TypeOf(Set(a))
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Set"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("TypeOf"), vec![Type::Set(Box::new(a))]),
                    constraints: vec![],
                },
            );
        }
        {
            // `Channel` → forall a. TypeOf(Channel(a))
            let (a, av) = self.fresh_tv();
            env.define(
                intern("Channel"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic(intern("TypeOf"), vec![Type::Channel(Box::new(a))]),
                    constraints: vec![],
                },
            );
        }
        {
            // `Map` → forall k v. TypeOf(Map(k, v))
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("Map"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Generic(intern("TypeOf"), vec![Type::Map(Box::new(k), Box::new(v))]),
                    constraints: vec![],
                },
            );
        }

        // ── Per-module registrations ───────────────────────────────────
        self.register_list_builtins(env);
        self.register_string_builtins(env);
        self.register_int_builtins(env);
        self.register_float_builtins(env);
        self.register_map_builtins(env);
        self.register_set_builtins(env);
        self.register_result_builtins(env);
        self.register_option_builtins(env);
        self.register_io_builtins(env);
        self.register_fs_builtins(env);
        self.register_env_builtins(env);
        self.register_test_builtins(env);
        self.register_math_builtins(env);
        self.register_channel_builtins(env);
        self.register_time_builtins(env);
        self.register_http_builtins(env);
        self.register_postgres_builtins(env);
        self.register_bytes_builtins(env);
        self.register_crypto_builtins(env);
        self.register_encoding_builtins(env);
        self.register_uuid_builtins(env);
        #[cfg(feature = "tcp")]
        self.register_tcp_builtins(env);
        self.register_stream_builtins(env);
        self.register_toml_builtins(env);
        self.register_errors_builtins(env);
    }

    fn register_list_builtins(&mut self, env: &mut TypeEnv) {
        list::register(self, env);
    }

    fn register_string_builtins(&mut self, env: &mut TypeEnv) {
        string::register(self, env);
    }

    fn register_int_builtins(&mut self, env: &mut TypeEnv) {
        int::register(self, env);
    }

    fn register_float_builtins(&mut self, env: &mut TypeEnv) {
        float::register(self, env);
    }

    fn register_map_builtins(&mut self, env: &mut TypeEnv) {
        map::register(self, env);
    }

    fn register_set_builtins(&mut self, env: &mut TypeEnv) {
        set::register(self, env);
    }

    fn register_result_builtins(&mut self, env: &mut TypeEnv) {
        result::register(self, env);
    }

    fn register_option_builtins(&mut self, env: &mut TypeEnv) {
        option::register(self, env);
    }

    fn register_io_builtins(&mut self, env: &mut TypeEnv) {
        io::register(self, env);
    }

    fn register_fs_builtins(&mut self, env: &mut TypeEnv) {
        fs::register(self, env);
    }

    fn register_env_builtins(&mut self, env: &mut TypeEnv) {
        env::register(self, env);
    }

    fn register_test_builtins(&mut self, env: &mut TypeEnv) {
        test::register(self, env);
    }

    fn register_math_builtins(&mut self, env: &mut TypeEnv) {
        math::register(self, env);
    }

    fn register_channel_builtins(&mut self, env: &mut TypeEnv) {
        channel::register(self, env);
    }

    fn register_time_builtins(&mut self, env: &mut TypeEnv) {
        time::register(self, env);
    }

    fn register_http_builtins(&mut self, env: &mut TypeEnv) {
        http::register(self, env);
    }

    fn register_postgres_builtins(&mut self, env: &mut TypeEnv) {
        postgres::register(self, env);
    }

    fn register_bytes_builtins(&mut self, env: &mut TypeEnv) {
        bytes::register(self, env);
    }

    fn register_crypto_builtins(&mut self, env: &mut TypeEnv) {
        crypto::register(self, env);
    }

    fn register_encoding_builtins(&mut self, env: &mut TypeEnv) {
        encoding::register(self, env);
    }

    fn register_uuid_builtins(&mut self, env: &mut TypeEnv) {
        uuid::register(self, env);
    }

    #[cfg(feature = "tcp")]
    fn register_tcp_builtins(&mut self, env: &mut TypeEnv) {
        tcp::register(self, env);
    }

    fn register_stream_builtins(&mut self, env: &mut TypeEnv) {
        stream::register(self, env);
    }

    fn register_toml_builtins(&mut self, env: &mut TypeEnv) {
        toml::register(self, env);
    }

    fn register_errors_builtins(&mut self, env: &mut TypeEnv) {
        errors::register(self, env);
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::super::*;

    // ── Builtin registration completeness ───────────────────────────

    #[test]
    fn test_register_builtins_populates_env() {
        let mut checker = TypeChecker::new();
        let mut env = TypeEnv::new();
        checker.register_builtins(&mut env);
        // Core functions should be registered
        assert!(
            env.lookup(intern("print")).is_some(),
            "print not registered"
        );
        assert!(
            env.lookup(intern("println")).is_some(),
            "println not registered"
        );
        assert!(
            env.lookup(intern("panic")).is_some(),
            "panic not registered"
        );
        assert!(env.lookup(intern("Some")).is_some(), "Some not registered");
        assert!(env.lookup(intern("None")).is_some(), "None not registered");
    }

    #[test]
    fn test_builtin_type_signatures_returns_qualified_names() {
        let sigs = builtin_type_signatures();
        assert!(
            sigs.contains_key("list.map"),
            "list.map missing from signatures"
        );
        assert!(
            sigs.contains_key("string.split"),
            "string.split missing from signatures"
        );
        assert!(
            sigs.contains_key("math.sqrt"),
            "math.sqrt missing from signatures"
        );
        // Should not contain unqualified names
        assert!(
            !sigs.contains_key("print"),
            "unqualified 'print' should not be in qualified signatures"
        );
    }

    // ── Math module ─────────────────────────────────────────────────

    #[test]
    fn test_math_sqrt() {
        assert_no_errors(
            r#"
import math
fn main() {
  let x = math.sqrt(4.0)
  x
}
        "#,
        );
    }

    #[test]
    fn test_math_trig_functions() {
        assert_no_errors(
            r#"
import math
fn main() {
  let a = math.sin(1.0)
  let b = math.cos(1.0)
  let c = math.tan(1.0)
  a + b + c
}
        "#,
        );
    }

    #[test]
    fn test_math_pow() {
        assert_no_errors(
            r#"
import math
fn main() {
  math.pow(2.0, 10.0)
}
        "#,
        );
    }

    // ── Time module ─────────────────────────────────────────────────

    #[test]
    fn test_time_now() {
        assert_no_errors(
            r#"
import time
fn main() {
  time.now()
}
        "#,
        );
    }

    #[test]
    fn test_time_sleep() {
        assert_no_errors(
            r#"
import time
fn main() {
  time.sleep(time.ms(100))
}
        "#,
        );
    }

    // ── HTTP module ─────────────────────────────────────────────────

    #[test]
    fn test_http_get_type() {
        assert_no_errors(
            r#"
import http
fn main() {
  http.get("http://example.com")
}
        "#,
        );
    }

    // ── FS module ───────────────────────────────────────────────────

    #[test]
    fn test_fs_exists_type_check() {
        assert_no_errors(
            r#"
import fs
fn main() {
  fs.exists("file.txt")
}
        "#,
        );
    }

    // ── Test module ─────────────────────────────────────────────────

    #[test]
    fn test_test_module_assert_eq() {
        assert_no_errors(
            r#"
import test
fn main() {
  test.assert_eq(1, 1)
}
        "#,
        );
    }

    // ── Builtin type mismatches ─────────────────────────────────────

    #[test]
    fn test_math_sqrt_wrong_type() {
        assert_has_error(
            r#"
import math
fn main() {
  math.sqrt("hello")
}
        "#,
            "type mismatch",
        );
    }
}
