//! Builtin type registrations for the standard library modules.
//!
//! Each register_*_builtins method populates the type environment with
//! type signatures for the corresponding standard library module.

use super::*;

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

        // ── panic: a -> b where a: Display (never returns) ─────────────
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("panic"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(vec![a], Box::new(b)),
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

        // ── json module ─────────────────────────────────────────────────

        // json.parse: (TypeOf(T), String) -> Result(T, String)
        // The first arg is a type descriptor (represented as TypeOf(T) so
        // that it cannot be confused with a value of the underlying type —
        // T2 audit fix). The carried type flows into the Result.
        {
            let (a, av) = self.fresh_tv();
            let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
            let result_ty = Type::Generic(intern("Result"), vec![a, Type::String]);
            env.define(
                intern("json.parse"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![descriptor_ty, Type::String], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_list: (TypeOf(T), String) -> Result(List(T), String)
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
                    ty: Type::Fun(vec![descriptor_ty, Type::String], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_map: (TypeOf(V), String) -> Result(Map(String, V), String)
        {
            let (a, av) = self.fresh_tv();
            let descriptor_ty = Type::Generic(intern("TypeOf"), vec![a.clone()]);
            let result_ty = Type::Generic(
                intern("Result"),
                vec![
                    Type::Map(Box::new(Type::String), Box::new(a)),
                    Type::String,
                ],
            );
            env.define(
                intern("json.parse_map"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![descriptor_ty, Type::String], Box::new(result_ty)),
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
    }

    fn register_list_builtins(&mut self, env: &mut TypeEnv) {
        // list.map: (List(a), (a -> b)) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(b.clone())),
                        ],
                        Box::new(Type::List(Box::new(b))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.filter: (List(a), (a -> Bool)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.filter"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.fold: (List(a), b, (b, a) -> b) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.fold"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            b.clone(),
                            Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                        ],
                        Box::new(b),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.each: (List(a), (a -> ())) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.each"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::Unit)),
                        ],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.find: (List(a), (a -> Bool)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.find"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Generic(intern("Option"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.zip: (List(a), List(b)) -> List((a, b))
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.zip"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::List(Box::new(b.clone())),
                        ],
                        Box::new(Type::List(Box::new(Type::Tuple(vec![a, b])))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.flatten: (List(List(a))) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.flatten"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(Type::List(Box::new(a.clone()))))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.sort_by: (List(a), (a -> b)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.sort_by"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(b)),
                        ],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.flat_map: (List(a), (a -> List(b))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.flat_map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::List(Box::new(b.clone())))),
                        ],
                        Box::new(Type::List(Box::new(b))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.filter_map: (List(a), (a -> Option(b))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.filter_map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic(intern("Option"), vec![b.clone()])),
                            ),
                        ],
                        Box::new(Type::List(Box::new(b))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.any: (List(a), (a -> Bool)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.any"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.all: (List(a), (a -> Bool)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.all"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.fold_until: (List(a), b, (b, a) -> Step(b)) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.fold_until"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            b.clone(),
                            Type::Fun(
                                vec![b.clone(), a],
                                Box::new(Type::Generic(intern("Step"), vec![b.clone()])),
                            ),
                        ],
                        Box::new(b),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.unfold: (a, (a) -> Option((b, a))) -> List(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("list.unfold"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            a.clone(),
                            Type::Fun(
                                vec![a.clone()],
                                Box::new(Type::Generic(
                                    intern("Option"),
                                    vec![Type::Tuple(vec![b.clone(), a])],
                                )),
                            ),
                        ],
                        Box::new(Type::List(Box::new(b))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.append: (List(a), a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.append"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), a.clone()],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.prepend: (List(a), a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.prepend"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), a.clone()],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.concat: (List(a), List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.concat"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::List(Box::new(a.clone())),
                        ],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.get: (List(a), Int) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.get"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), Type::Int],
                        Box::new(Type::Generic(intern("Option"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.set: (List(a), Int, a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.set"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), Type::Int, a.clone()],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.take: (List(a), Int) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.take"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), Type::Int],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.drop: (List(a), Int) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.drop"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), Type::Int],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.enumerate: (List(a)) -> List((Int, a))
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.enumerate"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(Type::Tuple(vec![Type::Int, a])))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.head: (List(a)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.head"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("Option"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.tail: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.tail"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.last: (List(a)) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.last"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("Option"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.reverse: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.reverse"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.sort: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.sort"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.unique: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.unique"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.contains: (List(a), a) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.contains"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), a],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.length: (List(a)) -> Int
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("list.length"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::List(Box::new(a))], Box::new(Type::Int)),
                    constraints: vec![],
                },
            );
        }

        // list.group_by: (List(a), (a -> k)) -> Map(k, List(a))
        {
            let (a, av) = self.fresh_tv();
            let (k, kv) = self.fresh_tv();
            env.define(
                intern("list.group_by"),
                Scheme {
                    vars: vec![av, kv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(k.clone())),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(Type::List(Box::new(a))))),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_string_builtins(&mut self, env: &mut TypeEnv) {
        // string.from: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("string.from"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // string.split: (String, String) -> List(String)
        env.define(
            intern("string.split"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // string.join: (List(String), String) -> String
        env.define(
            intern("string.join"),
            Scheme::mono(Type::Fun(
                vec![Type::List(Box::new(Type::String)), Type::String],
                Box::new(Type::String),
            )),
        );

        // string.trim: (String) -> String
        env.define(
            intern("string.trim"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.trim_start: (String) -> String
        env.define(
            intern("string.trim_start"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.trim_end: (String) -> String
        env.define(
            intern("string.trim_end"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.char_code: (String) -> Int
        env.define(
            intern("string.char_code"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.from_char_code: (Int) -> String
        env.define(
            intern("string.from_char_code"),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
        );

        // string.contains: (String, String) -> Bool
        env.define(
            intern("string.contains"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.replace: (String, String, String) -> String
        env.define(
            intern("string.replace"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.length: (String) -> Int
        env.define(
            intern("string.length"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.byte_length: (String) -> Int
        env.define(
            intern("string.byte_length"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.to_upper: (String) -> String
        env.define(
            intern("string.to_upper"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.to_lower: (String) -> String
        env.define(
            intern("string.to_lower"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.starts_with: (String, String) -> Bool
        env.define(
            intern("string.starts_with"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.ends_with: (String, String) -> Bool
        env.define(
            intern("string.ends_with"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.chars: (String) -> List(String)
        env.define(
            intern("string.chars"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // string.repeat: (String, Int) -> String
        env.define(
            intern("string.repeat"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int],
                Box::new(Type::String),
            )),
        );

        // string.index_of: (String, String) -> Option(Int)
        env.define(
            intern("string.index_of"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(intern("Option"), vec![Type::Int])),
            )),
        );

        // string.slice: (String, Int, Int) -> String
        env.define(
            intern("string.slice"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::Int],
                Box::new(Type::String),
            )),
        );

        // string.pad_left: (String, Int, String) -> String
        env.define(
            intern("string.pad_left"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.pad_right: (String, Int, String) -> String
        env.define(
            intern("string.pad_right"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.is_empty: (String) -> Bool
        env.define(
            intern("string.is_empty"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_alpha: (String) -> Bool
        env.define(
            intern("string.is_alpha"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_digit: (String) -> Bool
        env.define(
            intern("string.is_digit"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_upper: (String) -> Bool
        env.define(
            intern("string.is_upper"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_lower: (String) -> Bool
        env.define(
            intern("string.is_lower"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_alnum: (String) -> Bool
        env.define(
            intern("string.is_alnum"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_whitespace: (String) -> Bool
        env.define(
            intern("string.is_whitespace"),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );
    }

    fn register_int_builtins(&mut self, env: &mut TypeEnv) {
        // int.parse: (String) -> Result(Int, String)
        env.define(
            intern("int.parse"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::Int, Type::String],
                )),
            )),
        );

        // int.abs: (Int) -> Int
        env.define(
            intern("int.abs"),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Int))),
        );

        // int.min: (Int, Int) -> Int
        env.define(
            intern("int.min"),
            Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
        );

        // int.max: (Int, Int) -> Int
        env.define(
            intern("int.max"),
            Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
        );

        // int.to_float: (Int) -> Float
        env.define(
            intern("int.to_float"),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Float))),
        );

        // int.to_string: (Int) -> String
        env.define(
            intern("int.to_string"),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
        );
    }

    fn register_float_builtins(&mut self, env: &mut TypeEnv) {
        // float.parse: (String) -> Result(Float, String)
        env.define(
            intern("float.parse"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::Float, Type::String],
                )),
            )),
        );

        // float.round: (Float) -> Float
        env.define(
            intern("float.round"),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.ceil: (Float) -> Float
        env.define(
            intern("float.ceil"),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.floor: (Float) -> Float
        env.define(
            intern("float.floor"),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.abs: (Float) -> Float
        env.define(
            intern("float.abs"),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.min: (Float, Float) -> Float
        env.define(
            intern("float.min"),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            )),
        );

        // float.max: (Float, Float) -> Float
        env.define(
            intern("float.max"),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            )),
        );

        // float.to_string: (Float, Int) -> String
        // The second argument (decimal places) is optional at runtime: the
        // 1-arg form uses a shortest round-trippable representation, and
        // the 2-arg form formats with a fixed number of decimal places.
        // Registering the 2-arg form lets the typechecker validate both
        // arguments; the 1-arg call still passes the arity check because
        // module-qualified calls go through FieldAccess which permits ±1,
        // and the runtime honours that tolerance to match.
        env.define(
            intern("float.to_string"),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Int],
                Box::new(Type::String),
            )),
        );

        // float.to_int: (Float) -> Int
        env.define(
            intern("float.to_int"),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Int))),
        );
    }

    fn register_map_builtins(&mut self, env: &mut TypeEnv) {
        // map.get: (Map(k, v), k) -> Option(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.get"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v.clone())), k],
                        Box::new(Type::Generic(intern("Option"), vec![v])),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.set: (Map(k, v), k, v) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.set"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            k.clone(),
                            v.clone(),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.delete: (Map(k, v), k) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.delete"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            k.clone(),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.contains: (Map(k, v), k) -> Bool  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.contains"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v)), k],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.keys: (Map(k, v)) -> List(k)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.keys"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v))],
                        Box::new(Type::List(Box::new(k))),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.values: (Map(k, v)) -> List(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.values"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k), Box::new(v.clone()))],
                        Box::new(Type::List(Box::new(v))),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.merge: (Map(k, v), Map(k, v)) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.merge"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.length: (Map(k, v)) -> Int  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.length"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k), Box::new(v))],
                        Box::new(Type::Int),
                    ),
                    constraints: vec![(kv, intern("Hash"))],
                },
            );
        }

        // map.filter: (Map(k, v), (k, v) -> Bool) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.filter"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            Type::Fun(vec![k.clone(), v.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // map.map: (Map(k, v), (k, v) -> (k2, v2)) -> Map(k2, v2)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            let (k2, k2v) = self.fresh_tv();
            let (v2, v2v) = self.fresh_tv();
            env.define(
                intern("map.map"),
                Scheme {
                    vars: vec![kv, vv, k2v, v2v],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            Type::Fun(
                                vec![k, v],
                                Box::new(Type::Tuple(vec![k2.clone(), v2.clone()])),
                            ),
                        ],
                        Box::new(Type::Map(Box::new(k2), Box::new(v2))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // map.entries: (Map(k, v)) -> List((k, v))
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.entries"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v.clone()))],
                        Box::new(Type::List(Box::new(Type::Tuple(vec![k, v])))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // map.from_entries: (List((k, v))) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.from_entries"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(Type::Tuple(vec![
                            k.clone(),
                            v.clone(),
                        ])))],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // map.each: (Map(k, v), (k, v) -> ()) -> ()
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.each"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            Type::Fun(vec![k, v], Box::new(Type::Unit)),
                        ],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // map.update: (Map(k, v), k, v, (v) -> v) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                intern("map.update"),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            k.clone(),
                            v.clone(),
                            Type::Fun(vec![v.clone()], Box::new(v.clone())),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_set_builtins(&mut self, env: &mut TypeEnv) {
        // set.new: () -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.new"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![], Box::new(Type::Set(Box::new(a)))),
                    constraints: vec![],
                },
            );
        }

        // set.from_list: (List(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.from_list"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.to_list: (Set(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.to_list"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Set(Box::new(a.clone()))],
                        Box::new(Type::List(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.contains: (Set(a), a) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.contains"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Set(Box::new(a.clone())), a],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.insert: (Set(a), a) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.insert"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Set(Box::new(a.clone())), a.clone()],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.remove: (Set(a), a) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.remove"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Set(Box::new(a.clone())), a.clone()],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.length: (Set(a)) -> Int
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.length"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::Set(Box::new(a))], Box::new(Type::Int)),
                    constraints: vec![],
                },
            );
        }

        // set.union: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.union"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Set(Box::new(a.clone())),
                        ],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.intersection: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.intersection"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Set(Box::new(a.clone())),
                        ],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.difference: (Set(a), Set(a)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.difference"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Set(Box::new(a.clone())),
                        ],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.is_subset: (Set(a), Set(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.is_subset"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Set(Box::new(a.clone())),
                        ],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.map: (Set(a), (a -> b)) -> Set(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("set.map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(b.clone())),
                        ],
                        Box::new(Type::Set(Box::new(b))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.filter: (Set(a), (a -> Bool)) -> Set(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.filter"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Set(Box::new(a))),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.each: (Set(a), (a -> ())) -> ()
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("set.each"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(Type::Unit)),
                        ],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // set.fold: (Set(a), b, (b, a) -> b) -> b
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("set.fold"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Set(Box::new(a.clone())),
                            b.clone(),
                            Type::Fun(vec![b.clone(), a], Box::new(b.clone())),
                        ],
                        Box::new(b),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_result_builtins(&mut self, env: &mut TypeEnv) {
        // result.map_ok: (Result(a,e), (a -> b)) -> Result(b,e)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.map_ok"),
                Scheme {
                    vars: vec![av, bv, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Result"), vec![a, e.clone()]),
                            Type::Fun(vec![Type::Var(av)], Box::new(b.clone())),
                        ],
                        Box::new(Type::Generic(intern("Result"), vec![b, e])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.unwrap_or: (Result(a,e), a) -> a
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.unwrap_or"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Result"), vec![a.clone(), e]),
                            a.clone(),
                        ],
                        Box::new(a),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.map_err: (Result(a,e), (e -> f)) -> Result(a,f)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            let (f, fv) = self.fresh_tv();
            env.define(
                intern("result.map_err"),
                Scheme {
                    vars: vec![av, ev, fv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                            Type::Fun(vec![e], Box::new(f.clone())),
                        ],
                        Box::new(Type::Generic(intern("Result"), vec![a, f])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.flatten: (Result(Result(a,e),e)) -> Result(a,e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.flatten"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic(
                            intern("Result"),
                            vec![
                                Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                                e.clone(),
                            ],
                        )],
                        Box::new(Type::Generic(intern("Result"), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.flat_map: (Result(a, e), (a) -> Result(b, e)) -> Result(b, e)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.flat_map"),
                Scheme {
                    vars: vec![av, bv, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Result"), vec![a.clone(), e.clone()]),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic(
                                    intern("Result"),
                                    vec![b.clone(), e.clone()],
                                )),
                            ),
                        ],
                        Box::new(Type::Generic(intern("Result"), vec![b, e])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.is_ok: (Result(a,e)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.is_ok"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Result"), vec![a, e])],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // result.is_err: (Result(a,e)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("result.is_err"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Result"), vec![a, e])],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_option_builtins(&mut self, env: &mut TypeEnv) {
        // option.map: (Option(a), (a -> b)) -> Option(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("option.map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Option"), vec![a.clone()]),
                            Type::Fun(vec![a], Box::new(b.clone())),
                        ],
                        Box::new(Type::Generic(intern("Option"), vec![b])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.flat_map: (Option(a), (a -> Option(b))) -> Option(b)
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("option.flat_map"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic(intern("Option"), vec![a.clone()]),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic(intern("Option"), vec![b.clone()])),
                            ),
                        ],
                        Box::new(Type::Generic(intern("Option"), vec![b])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.unwrap_or: (Option(a), a) -> a
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("option.unwrap_or"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Option"), vec![a.clone()]), a.clone()],
                        Box::new(a),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.to_result: (Option(a), e) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                intern("option.to_result"),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Option"), vec![a.clone()]), e.clone()],
                        Box::new(Type::Generic(intern("Result"), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.is_some: (Option(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("option.is_some"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Option"), vec![a])],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.is_none: (Option(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("option.is_none"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic(intern("Option"), vec![a])],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_io_builtins(&mut self, env: &mut TypeEnv) {
        // io.inspect: a -> String
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("io.inspect"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // io.read_file: (String) -> Result(String, String)
        env.define(
            intern("io.read_file"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::String, Type::String],
                )),
            )),
        );

        // io.write_file: (String, String) -> Result((), String)
        env.define(
            intern("io.write_file"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::Unit, Type::String],
                )),
            )),
        );

        // io.read_line: () -> Result(String, String)
        env.define(
            intern("io.read_line"),
            Scheme::mono(Type::Fun(
                vec![],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::String, Type::String],
                )),
            )),
        );

        // io.args: () -> List(String)
        env.define(
            intern("io.args"),
            Scheme::mono(Type::Fun(
                vec![],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );
    }

    fn register_fs_builtins(&mut self, env: &mut TypeEnv) {
        // fs.exists / fs.is_file / fs.is_dir: (String) -> Bool
        let string_to_bool = Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool)));
        for name in &["fs.exists", "fs.is_file", "fs.is_dir"] {
            env.define(intern(name), string_to_bool.clone());
        }

        // fs.list_dir: (String) -> Result(List(String), String)
        env.define(
            intern("fs.list_dir"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    intern("Result"),
                    vec![Type::List(Box::new(Type::String)), Type::String],
                )),
            )),
        );

        // fs.mkdir / fs.remove: (String) -> Result(Unit, String)
        let string_to_result = Scheme::mono(Type::Fun(
            vec![Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Unit, Type::String],
            )),
        ));
        for name in &["fs.mkdir", "fs.remove"] {
            env.define(intern(name), string_to_result.clone());
        }

        // fs.rename / fs.copy: (String, String) -> Result(Unit, String)
        let ss_to_result = Scheme::mono(Type::Fun(
            vec![Type::String, Type::String],
            Box::new(Type::Generic(
                intern("Result"),
                vec![Type::Unit, Type::String],
            )),
        ));
        for name in &["fs.rename", "fs.copy"] {
            env.define(intern(name), ss_to_result.clone());
        }
    }

    fn register_env_builtins(&mut self, env: &mut TypeEnv) {
        // env.get: (String) -> Option(String)
        env.define(
            intern("env.get"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(intern("Option"), vec![Type::String])),
            )),
        );

        // env.set: (String, String) -> Unit
        env.define(
            intern("env.set"),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Unit),
            )),
        );
    }

    fn register_test_builtins(&mut self, env: &mut TypeEnv) {
        // test.assert: (Bool, String) -> ()
        // The message parameter is optional at runtime; registering the full
        // arity lets the typechecker validate the message type while the
        // is_method_call arity tolerance still allows the 1-arg form.
        env.define(
            intern("test.assert"),
            Scheme::mono(Type::Fun(
                vec![Type::Bool, Type::String],
                Box::new(Type::Unit),
            )),
        );

        // test.assert_eq: (a, a, String) -> ()
        // The message parameter is optional at runtime.
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("test.assert_eq"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // test.assert_ne: (a, a, String) -> ()
        // The message parameter is optional at runtime.
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("test.assert_ne"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone(), a, Type::String], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_math_builtins(&mut self, env: &mut TypeEnv) {
        // Functions that can produce non-finite results: (Float) -> ExtFloat
        {
            let float_to_extfloat =
                Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::ExtFloat)));
            for name in &[
                "math.sqrt",
                "math.log",
                "math.log10",
                "math.asin",
                "math.acos",
                "math.exp",
            ] {
                env.define(intern(name), float_to_extfloat.clone());
            }
        }

        // Functions that always produce finite results: (Float) -> Float
        {
            let float_to_float = Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float)));
            for name in &["math.sin", "math.cos", "math.tan", "math.atan"] {
                env.define(intern(name), float_to_float.clone());
            }
        }

        // math.pow: (Float, Float) -> ExtFloat (can overflow)
        {
            let ff_to_ef = Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::ExtFloat),
            ));
            env.define(intern("math.pow"), ff_to_ef);
        }

        // math.atan2: (Float, Float) -> Float (always finite)
        {
            let ff_to_f = Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            ));
            env.define(intern("math.atan2"), ff_to_f);
        }

        // math.random: () -> Float
        env.define(
            intern("math.random"),
            Scheme::mono(Type::Fun(vec![], Box::new(Type::Float))),
        );

        // Math constants
        env.define(intern("math.pi"), Scheme::mono(Type::Float));
        env.define(intern("math.e"), Scheme::mono(Type::Float));

        // Float constants
        env.define(intern("float.max_value"), Scheme::mono(Type::Float));
        env.define(intern("float.min_value"), Scheme::mono(Type::Float));
        env.define(intern("float.epsilon"), Scheme::mono(Type::Float));
        env.define(intern("float.min_positive"), Scheme::mono(Type::Float));
        env.define(intern("float.infinity"), Scheme::mono(Type::ExtFloat));
        env.define(intern("float.neg_infinity"), Scheme::mono(Type::ExtFloat));
        env.define(intern("float.nan"), Scheme::mono(Type::ExtFloat));
    }

    fn register_channel_builtins(&mut self, env: &mut TypeEnv) {
        // channel.new: (Int) -> Channel(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.new"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::Int], Box::new(Type::Channel(Box::new(a)))),
                    constraints: vec![],
                },
            );
        }

        // channel.send: (Channel(a), a) -> Unit
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.send"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Channel(Box::new(a.clone())), a],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.receive: (Channel(a)) -> ChannelResult(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.receive"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Channel(Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("ChannelResult"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.close: (Channel(a)) -> Unit
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.close"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::Channel(Box::new(a))], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // channel.try_send: (Channel(a), a) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.try_send"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Channel(Box::new(a.clone())), a],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.try_receive: (Channel(a)) -> ChannelResult(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.try_receive"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Channel(Box::new(a.clone()))],
                        Box::new(Type::Generic(intern("ChannelResult"), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.select: (List(Channel(a))) -> (Channel(a), ChannelResult(a))
        {
            let (a, av) = self.fresh_tv();
            let ch_a = Type::Channel(Box::new(a.clone()));
            env.define(
                intern("channel.select"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(ch_a.clone()))],
                        Box::new(Type::Tuple(vec![
                            ch_a,
                            Type::Generic(intern("ChannelResult"), vec![a]),
                        ])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.each: (Channel(a), (a) -> b) -> Unit
        {
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                intern("channel.each"),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Channel(Box::new(a.clone())),
                            Type::Fun(vec![a], Box::new(b)),
                        ],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.timeout: (Int) -> Channel(a)
        //
        // Creates a channel that automatically closes after the given number
        // of milliseconds. The returned channel carries no values -- the
        // runtime never sends on it, it just closes it when the deadline
        // elapses. A polymorphic element type lets the result be mixed into
        // a `channel.select` alongside channels of any element type (the
        // element will never actually be observed because the channel closes
        // before any `Message` arrives).
        {
            let (a, av) = self.fresh_tv();
            env.define(
                intern("channel.timeout"),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::Int], Box::new(Type::Channel(Box::new(a)))),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_time_builtins(&mut self, env: &mut TypeEnv) {
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
        self.records.insert(
            intern("Instant"),
            RecordInfo {
                _name: intern("Instant"),
                _params: vec![],
                fields: vec![(intern("epoch_ns"), Type::Int)],
            },
        );
        self.records.insert(
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
        self.records.insert(
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
        self.records.insert(
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
        self.records.insert(
            intern("Duration"),
            RecordInfo {
                _name: intern("Duration"),
                _params: vec![],
                fields: vec![(intern("ns"), Type::Int)],
            },
        );

        // Register Weekday enum
        self.enums.insert(
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
            self.variant_to_enum.insert(intern(day), intern("Weekday"));
            env.define(intern(day), Scheme::mono(weekday_ty.clone()));
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

        // ── Register Display (and other builtin traits) for time types ──
        {
            let dummy_span = Span {
                line: 0,
                col: 0,
                offset: 0,
            };
            let time_type_names = ["Instant", "Date", "Time", "DateTime", "Duration", "Weekday"];
            let all_traits = ["Equal", "Compare", "Hash", "Display"];
            let trait_methods: &[(&str, Type)] = &[
                (
                    "display",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::String)),
                ),
                (
                    "equal",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Bool),
                    ),
                ),
                (
                    "compare",
                    Type::Fun(
                        vec![self.fresh_var(), self.fresh_var()],
                        Box::new(Type::Int),
                    ),
                ),
                (
                    "hash",
                    Type::Fun(vec![self.fresh_var()], Box::new(Type::Int)),
                ),
            ];
            for type_name in &time_type_names {
                for trait_name in &all_traits {
                    self.trait_impl_set
                        .insert((intern(trait_name), intern(type_name)));
                }
                for (method_name, method_type) in trait_methods {
                    self.method_table.insert(
                        (intern(type_name), intern(method_name)),
                        MethodEntry {
                            method_type: method_type.clone(),
                            span: dummy_span,
                            is_auto_derived: true,
                            trait_name: None,
                            method_constraints: Vec::new(),
                        },
                    );
                }
            }
        }

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
                    vec![date_ty.clone(), Type::String],
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
                    vec![time_of_day_ty.clone(), Type::String],
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
                    vec![datetime_ty.clone(), Type::String],
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
                    vec![date_ty.clone(), Type::String],
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

    fn register_http_builtins(&mut self, env: &mut TypeEnv) {
        // ── HTTP module type definitions ─────────────────────────────

        // Method enum
        let method_ty = Type::Generic(intern("Method"), vec![]);

        self.enums.insert(
            intern("Method"),
            EnumInfo {
                _name: intern("Method"),
                params: vec![],
                param_var_ids: vec![],
                variants: vec![
                    VariantInfo {
                        name: intern("GET"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("POST"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("PUT"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("PATCH"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("DELETE"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("HEAD"),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: intern("OPTIONS"),
                        field_types: vec![],
                    },
                ],
            },
        );
        for variant in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            self.variant_to_enum
                .insert(intern(variant), intern("Method"));
            env.define(intern(variant), Scheme::mono(method_ty.clone()));
        }

        // Response record
        let map_ss = Type::Map(Box::new(Type::String), Box::new(Type::String));

        let response_ty = Type::Record(
            intern("Response"),
            vec![
                (intern("status"), Type::Int),
                (intern("body"), Type::String),
                (intern("headers"), map_ss.clone()),
            ],
        );

        self.records.insert(
            intern("Response"),
            RecordInfo {
                _name: intern("Response"),
                _params: vec![],
                fields: vec![
                    (intern("status"), Type::Int),
                    (intern("body"), Type::String),
                    (intern("headers"), map_ss.clone()),
                ],
            },
        );

        // Request record
        let request_ty = Type::Record(
            intern("Request"),
            vec![
                (intern("method"), method_ty.clone()),
                (intern("path"), Type::String),
                (intern("query"), Type::String),
                (intern("headers"), map_ss.clone()),
                (intern("body"), Type::String),
            ],
        );

        self.records.insert(
            intern("Request"),
            RecordInfo {
                _name: intern("Request"),
                _params: vec![],
                fields: vec![
                    (intern("method"), method_ty.clone()),
                    (intern("path"), Type::String),
                    (intern("query"), Type::String),
                    (intern("headers"), map_ss.clone()),
                    (intern("body"), Type::String),
                ],
            },
        );

        // ── Function signatures ──────────────────────────────────────

        let result_response =
            Type::Generic(intern("Result"), vec![response_ty.clone(), Type::String]);

        // http.get: (String) -> Result(Response, String)
        env.define(
            intern("http.get"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(result_response.clone()),
            )),
        );

        // http.request: (Method, String, String, Map(String, String)) -> Result(Response, String)
        env.define(
            intern("http.request"),
            Scheme::mono(Type::Fun(
                vec![method_ty, Type::String, Type::String, map_ss],
                Box::new(result_response),
            )),
        );

        // http.serve: (Int, Fn(Request) -> Response) -> Unit
        env.define(
            intern("http.serve"),
            Scheme::mono(Type::Fun(
                vec![
                    Type::Int,
                    Type::Fun(vec![request_ty], Box::new(response_ty)),
                ],
                Box::new(Type::Unit),
            )),
        );

        // http.segments: (String) -> List(String)
        env.define(
            intern("http.segments"),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn assert_no_errors(input: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        let hard: Vec<_> = errors
            .iter()
            .filter(|e| e.severity == Severity::Error)
            .collect();
        assert!(
            hard.is_empty(),
            "expected no type errors, got:\n{}",
            hard.iter()
                .map(|e| format!("  {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    fn assert_has_error(input: &str, expected: &str) {
        let tokens = crate::lexer::Lexer::new(input)
            .tokenize()
            .expect("lexer error");
        let mut program = crate::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse error");
        let errors = check(&mut program);
        assert!(
            errors.iter().any(|e| e.message.contains(expected)),
            "expected error containing '{expected}', got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

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
