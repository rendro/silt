//! Builtin type registrations for the standard library modules.
//!
//! Each register_*_builtins method populates the type environment with
//! type signatures for the corresponding standard library module.

use super::*;

impl TypeChecker {
    pub(super) fn register_builtins(&mut self, env: &mut TypeEnv) {
        // ── print / println: (a) -> () ─────────────────────────────────
        // Accept any type (the runtime uses Display for formatting).
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "print".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "println".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a.clone()], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // ── panic: String -> a ─────────────────────────────────────────
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "panic".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![Type::String], Box::new(a)),
                    constraints: vec![],
                },
            );
        }

        // ── Variant constructors ───────────────────────────────────────

        // Ok(a) -> Result(a, e)
        {
            let (a, av) = self.fresh_tv();
            let (e, ev) = self.fresh_tv();
            env.define(
                "Ok".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
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
                "Err".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![e.clone()],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }
        // Some(a) -> Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Some".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        // None : Option(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "None".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic("Option".into(), vec![a]),
                    constraints: vec![],
                },
            );
        }

        // ── Builtin enum info for Option and Result ────────────────────

        self.enums.insert(
            "Option".into(),
            EnumInfo {
                _name: "Option".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo {
                        name: "Some".into(),
                        field_types: vec![Type::Var(0)], // placeholder
                    },
                    VariantInfo {
                        name: "None".into(),
                        field_types: vec![],
                    },
                ],
            },
        );
        self.variant_to_enum.insert("Some".into(), "Option".into());
        self.variant_to_enum.insert("None".into(), "Option".into());

        self.enums.insert(
            "Result".into(),
            EnumInfo {
                _name: "Result".into(),
                params: vec!["a".into(), "e".into()],
                variants: vec![
                    VariantInfo {
                        name: "Ok".into(),
                        field_types: vec![Type::Var(0)], // placeholder
                    },
                    VariantInfo {
                        name: "Err".into(),
                        field_types: vec![Type::Var(1)], // placeholder
                    },
                ],
            },
        );
        self.variant_to_enum.insert("Ok".into(), "Result".into());
        self.variant_to_enum.insert("Err".into(), "Result".into());

        // Step enum: Stop(a) / Continue(a) — for list.fold_until
        self.enums.insert(
            "Step".into(),
            EnumInfo {
                _name: "Step".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo {
                        name: "Stop".into(),
                        field_types: vec![Type::Var(0)],
                    },
                    VariantInfo {
                        name: "Continue".into(),
                        field_types: vec![Type::Var(0)],
                    },
                ],
            },
        );
        self.variant_to_enum.insert("Stop".into(), "Step".into());
        self.variant_to_enum
            .insert("Continue".into(), "Step".into());
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Stop".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Step".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Continue".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("Step".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // ChannelResult enum: Message(a) / Closed — for channel.receive
        self.enums.insert(
            "ChannelResult".into(),
            EnumInfo {
                _name: "ChannelResult".into(),
                params: vec!["a".into()],
                variants: vec![
                    VariantInfo {
                        name: "Message".into(),
                        field_types: vec![Type::Var(0)],
                    },
                    VariantInfo {
                        name: "Closed".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Sent".into(),
                        field_types: vec![],
                    },
                ],
            },
        );
        self.variant_to_enum
            .insert("Message".into(), "ChannelResult".into());
        self.variant_to_enum
            .insert("Closed".into(), "ChannelResult".into());
        // Also register Empty and Sent as standalones
        self.variant_to_enum
            .insert("Empty".into(), "ChannelResult".into());
        self.variant_to_enum
            .insert("Sent".into(), "ChannelResult".into());
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Message".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![a.clone()],
                        Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Closed".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic("ChannelResult".into(), vec![a]),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Empty".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic("ChannelResult".into(), vec![a]),
                    constraints: vec![],
                },
            );
        }
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "Sent".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Generic("ChannelResult".into(), vec![a]),
                    constraints: vec![],
                },
            );
        }

        // ── task module ────────────────────────────────────────────────

        // task.spawn: (() -> a) -> Handle
        {
            let (a, av) = self.fresh_tv();
            let (h, hv) = self.fresh_tv();
            env.define(
                "task.spawn".into(),
                Scheme {
                    vars: vec![av, hv],
                    ty: Type::Fun(vec![Type::Fun(vec![], Box::new(a))], Box::new(h)),
                    constraints: vec![],
                },
            );
        }

        // task.join: (Handle) -> a
        {
            let (h, hv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "task.join".into(),
                Scheme {
                    vars: vec![hv, av],
                    ty: Type::Fun(vec![h], Box::new(a)),
                    constraints: vec![],
                },
            );
        }

        // task.cancel: (Handle) -> Unit
        {
            let (h, hv) = self.fresh_tv();
            env.define(
                "task.cancel".into(),
                Scheme {
                    vars: vec![hv],
                    ty: Type::Fun(vec![h], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // ── regex module ────────────────────────────────────────────────

        // regex.is_match: (String, String) -> Bool
        env.define(
            "regex.is_match".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // regex.find: (String, String) -> Option(String)
        env.define(
            "regex.find".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic("Option".into(), vec![Type::String])),
            )),
        );

        // regex.find_all: (String, String) -> List(String)
        env.define(
            "regex.find_all".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // regex.split: (String, String) -> List(String)
        env.define(
            "regex.split".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // regex.replace: (String, String, String) -> String
        env.define(
            "regex.replace".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // regex.replace_all: (String, String, String) -> String
        env.define(
            "regex.replace_all".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // regex.replace_all_with: (String, String, (String) -> String) -> String
        env.define(
            "regex.replace_all_with".into(),
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
            "regex.captures".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    "Option".into(),
                    vec![Type::List(Box::new(Type::String))],
                )),
            )),
        );

        // regex.captures_all: (String, String) -> List(List(String))
        env.define(
            "regex.captures_all".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::List(Box::new(Type::String))))),
            )),
        );

        // ── json module ─────────────────────────────────────────────────

        // json.parse: (T, String) -> Result(T, String)
        // The first arg is a type descriptor; the same type flows into the Result.
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic("Result".into(), vec![a.clone(), Type::String]);
            env.define(
                "json.parse".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a, Type::String], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_list: (T, String) -> Result(List(T), String)
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic(
                "Result".into(),
                vec![Type::List(Box::new(a.clone())), Type::String],
            );
            env.define(
                "json.parse_list".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a, Type::String], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.parse_map: (V, String) -> Result(Map(String, V), String)
        {
            let (a, av) = self.fresh_tv();
            let result_ty = Type::Generic(
                "Result".into(),
                vec![
                    Type::Map(Box::new(Type::String), Box::new(a.clone())),
                    Type::String,
                ],
            );
            env.define(
                "json.parse_map".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a, Type::String], Box::new(result_ty)),
                    constraints: vec![],
                },
            );
        }

        // json.stringify: (a) -> String
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "json.stringify".into(),
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
                "json.pretty".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // ── Primitive type descriptors (for json.parse_map etc.) ──────
        // These carry the actual type so json.parse can propagate it
        // into the return type.
        for name in &["Int", "Float", "String", "Bool"] {
            let ty = match *name {
                "Int" => Type::Int,
                "Float" => Type::Float,
                "String" => Type::String,
                "Bool" => Type::Bool,
                _ => unreachable!(),
            };
            env.define(
                name.to_string(),
                Scheme {
                    vars: vec![],
                    ty,
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
                "list.map".into(),
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
                "list.filter".into(),
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
                "list.fold".into(),
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
                "list.each".into(),
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
                "list.find".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(vec![a.clone()], Box::new(Type::Bool)),
                        ],
                        Box::new(Type::Generic("Option".into(), vec![a])),
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
                "list.zip".into(),
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
                "list.flatten".into(),
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
                "list.sort_by".into(),
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
                "list.flat_map".into(),
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
                "list.filter_map".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic("Option".into(), vec![b.clone()])),
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
                "list.any".into(),
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
                "list.all".into(),
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
                "list.fold_until".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::List(Box::new(a.clone())),
                            b.clone(),
                            Type::Fun(
                                vec![b.clone(), a],
                                Box::new(Type::Generic("Step".into(), vec![b.clone()])),
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
                "list.unfold".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            a.clone(),
                            Type::Fun(
                                vec![a.clone()],
                                Box::new(Type::Generic(
                                    "Option".into(),
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
                "list.append".into(),
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
                "list.prepend".into(),
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
                "list.concat".into(),
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
                "list.get".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone())), Type::Int],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.set: (List(a), Int, a) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "list.set".into(),
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
                "list.take".into(),
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
                "list.drop".into(),
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
                "list.enumerate".into(),
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
                "list.head".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.tail: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "list.tail".into(),
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
                "list.last".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(a.clone()))],
                        Box::new(Type::Generic("Option".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // list.reverse: (List(a)) -> List(a)
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "list.reverse".into(),
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
                "list.sort".into(),
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
                "list.unique".into(),
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
                "list.contains".into(),
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
                "list.length".into(),
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
                "list.group_by".into(),
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
                "string.from".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // string.split: (String, String) -> List(String)
        env.define(
            "string.split".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // string.join: (List(String), String) -> String
        env.define(
            "string.join".into(),
            Scheme::mono(Type::Fun(
                vec![Type::List(Box::new(Type::String)), Type::String],
                Box::new(Type::String),
            )),
        );

        // string.trim: (String) -> String
        env.define(
            "string.trim".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.trim_start: (String) -> String
        env.define(
            "string.trim_start".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.trim_end: (String) -> String
        env.define(
            "string.trim_end".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.char_code: (String) -> Int
        env.define(
            "string.char_code".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.from_char_code: (Int) -> String
        env.define(
            "string.from_char_code".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
        );

        // string.contains: (String, String) -> Bool
        env.define(
            "string.contains".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.replace: (String, String, String) -> String
        env.define(
            "string.replace".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.length: (String) -> Int
        env.define(
            "string.length".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.byte_length: (String) -> Int
        env.define(
            "string.byte_length".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Int))),
        );

        // string.to_upper: (String) -> String
        env.define(
            "string.to_upper".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.to_lower: (String) -> String
        env.define(
            "string.to_lower".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::String))),
        );

        // string.starts_with: (String, String) -> Bool
        env.define(
            "string.starts_with".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.ends_with: (String, String) -> Bool
        env.define(
            "string.ends_with".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Bool),
            )),
        );

        // string.chars: (String) -> List(String)
        env.define(
            "string.chars".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );

        // string.repeat: (String, Int) -> String
        env.define(
            "string.repeat".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int],
                Box::new(Type::String),
            )),
        );

        // string.index_of: (String, String) -> Option(Int)
        env.define(
            "string.index_of".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic("Option".into(), vec![Type::Int])),
            )),
        );

        // string.slice: (String, Int, Int) -> String
        env.define(
            "string.slice".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::Int],
                Box::new(Type::String),
            )),
        );

        // string.pad_left: (String, Int, String) -> String
        env.define(
            "string.pad_left".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.pad_right: (String, Int, String) -> String
        env.define(
            "string.pad_right".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::Int, Type::String],
                Box::new(Type::String),
            )),
        );

        // string.is_empty: (String) -> Bool
        env.define(
            "string.is_empty".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_alpha: (String) -> Bool
        env.define(
            "string.is_alpha".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_digit: (String) -> Bool
        env.define(
            "string.is_digit".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_upper: (String) -> Bool
        env.define(
            "string.is_upper".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_lower: (String) -> Bool
        env.define(
            "string.is_lower".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_alnum: (String) -> Bool
        env.define(
            "string.is_alnum".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );

        // string.is_whitespace: (String) -> Bool
        env.define(
            "string.is_whitespace".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );
    }

    fn register_int_builtins(&mut self, env: &mut TypeEnv) {
        // int.parse: (String) -> Result(Int, String)
        env.define(
            "int.parse".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![Type::Int, Type::String],
                )),
            )),
        );

        // int.abs: (Int) -> Int
        env.define(
            "int.abs".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Int))),
        );

        // int.min: (Int, Int) -> Int
        env.define(
            "int.min".into(),
            Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
        );

        // int.max: (Int, Int) -> Int
        env.define(
            "int.max".into(),
            Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
        );

        // int.to_float: (Int) -> Float
        env.define(
            "int.to_float".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Float))),
        );

        // int.to_string: (Int) -> String
        env.define(
            "int.to_string".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::String))),
        );
    }

    fn register_float_builtins(&mut self, env: &mut TypeEnv) {
        // float.parse: (String) -> Result(Float, String)
        env.define(
            "float.parse".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![Type::Float, Type::String],
                )),
            )),
        );

        // float.round: (Float) -> Float
        env.define(
            "float.round".into(),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.ceil: (Float) -> Float
        env.define(
            "float.ceil".into(),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.floor: (Float) -> Float
        env.define(
            "float.floor".into(),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.abs: (Float) -> Float
        env.define(
            "float.abs".into(),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float))),
        );

        // float.min: (Float, Float) -> Float
        env.define(
            "float.min".into(),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            )),
        );

        // float.max: (Float, Float) -> Float
        env.define(
            "float.max".into(),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            )),
        );

        // float.to_string: (Float, Int) -> String
        // The second argument (decimal places) is optional at runtime;
        // registering the 2-arg form lets the typechecker validate both
        // arguments.  The 1-arg call still passes the arity check because
        // module-qualified calls go through FieldAccess which permits ±1.
        env.define(
            "float.to_string".into(),
            Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Int],
                Box::new(Type::String),
            )),
        );

        // float.to_int: (Float) -> Int
        env.define(
            "float.to_int".into(),
            Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Int))),
        );
    }

    fn register_map_builtins(&mut self, env: &mut TypeEnv) {
        // map.get: (Map(k, v), k) -> Option(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.get".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v.clone())), k],
                        Box::new(Type::Generic("Option".into(), vec![v])),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.set: (Map(k, v), k, v) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.set".into(),
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
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.delete: (Map(k, v), k) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.delete".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            k.clone(),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.contains: (Map(k, v), k) -> Bool  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.contains".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v)), k],
                        Box::new(Type::Bool),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.keys: (Map(k, v)) -> List(k)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.keys".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k.clone()), Box::new(v))],
                        Box::new(Type::List(Box::new(k))),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.values: (Map(k, v)) -> List(v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.values".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k), Box::new(v.clone()))],
                        Box::new(Type::List(Box::new(v))),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.merge: (Map(k, v), Map(k, v)) -> Map(k, v)  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.merge".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                            Type::Map(Box::new(k.clone()), Box::new(v.clone())),
                        ],
                        Box::new(Type::Map(Box::new(k), Box::new(v))),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.length: (Map(k, v)) -> Int  where k: Hash
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.length".into(),
                Scheme {
                    vars: vec![kv, vv],
                    ty: Type::Fun(
                        vec![Type::Map(Box::new(k), Box::new(v))],
                        Box::new(Type::Int),
                    ),
                    constraints: vec![(kv, "Hash".into())],
                },
            );
        }

        // map.filter: (Map(k, v), (k, v) -> Bool) -> Map(k, v)
        {
            let (k, kv) = self.fresh_tv();
            let (v, vv) = self.fresh_tv();
            env.define(
                "map.filter".into(),
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
                "map.map".into(),
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
                "map.entries".into(),
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
                "map.from_entries".into(),
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
                "map.each".into(),
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
                "map.update".into(),
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
                "set.new".into(),
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
                "set.from_list".into(),
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
                "set.to_list".into(),
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
                "set.contains".into(),
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
                "set.insert".into(),
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
                "set.remove".into(),
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
                "set.length".into(),
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
                "set.union".into(),
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
                "set.intersection".into(),
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
                "set.difference".into(),
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
                "set.is_subset".into(),
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
                "set.map".into(),
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
                "set.filter".into(),
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
                "set.each".into(),
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
                "set.fold".into(),
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
                "result.map_ok".into(),
                Scheme {
                    vars: vec![av, bv, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a, e.clone()]),
                            Type::Fun(vec![Type::Var(av)], Box::new(b.clone())),
                        ],
                        Box::new(Type::Generic("Result".into(), vec![b, e])),
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
                "result.unwrap_or".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a.clone(), e]),
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
                "result.map_err".into(),
                Scheme {
                    vars: vec![av, ev, fv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                            Type::Fun(vec![e], Box::new(f.clone())),
                        ],
                        Box::new(Type::Generic("Result".into(), vec![a, f])),
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
                "result.flatten".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic(
                            "Result".into(),
                            vec![
                                Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                                e.clone(),
                            ],
                        )],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
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
                "result.flat_map".into(),
                Scheme {
                    vars: vec![av, bv, ev],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Result".into(), vec![a.clone(), e.clone()]),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic(
                                    "Result".into(),
                                    vec![b.clone(), e.clone()],
                                )),
                            ),
                        ],
                        Box::new(Type::Generic("Result".into(), vec![b, e])),
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
                "result.is_ok".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic("Result".into(), vec![a, e])],
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
                "result.is_err".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic("Result".into(), vec![a, e])],
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
                "option.map".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Option".into(), vec![a.clone()]),
                            Type::Fun(vec![a], Box::new(b.clone())),
                        ],
                        Box::new(Type::Generic("Option".into(), vec![b])),
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
                "option.flat_map".into(),
                Scheme {
                    vars: vec![av, bv],
                    ty: Type::Fun(
                        vec![
                            Type::Generic("Option".into(), vec![a.clone()]),
                            Type::Fun(
                                vec![a],
                                Box::new(Type::Generic("Option".into(), vec![b.clone()])),
                            ),
                        ],
                        Box::new(Type::Generic("Option".into(), vec![b])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.unwrap_or: (Option(a), a) -> a
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "option.unwrap_or".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic("Option".into(), vec![a.clone()]), a.clone()],
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
                "option.to_result".into(),
                Scheme {
                    vars: vec![av, ev],
                    ty: Type::Fun(
                        vec![Type::Generic("Option".into(), vec![a.clone()]), e.clone()],
                        Box::new(Type::Generic("Result".into(), vec![a, e])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // option.is_some: (Option(a)) -> Bool
        {
            let (a, av) = self.fresh_tv();
            env.define(
                "option.is_some".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic("Option".into(), vec![a])],
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
                "option.is_none".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(
                        vec![Type::Generic("Option".into(), vec![a])],
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
                "io.inspect".into(),
                Scheme {
                    vars: vec![av],
                    ty: Type::Fun(vec![a], Box::new(Type::String)),
                    constraints: vec![],
                },
            );
        }

        // io.read_file: (String) -> Result(String, String)
        env.define(
            "io.read_file".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![Type::String, Type::String],
                )),
            )),
        );

        // io.write_file: (String, String) -> Result((), String)
        env.define(
            "io.write_file".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![Type::Unit, Type::String],
                )),
            )),
        );

        // io.read_line: () -> Result(String, String)
        env.define(
            "io.read_line".into(),
            Scheme::mono(Type::Fun(
                vec![],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![Type::String, Type::String],
                )),
            )),
        );

        // io.args: () -> List(String)
        env.define(
            "io.args".into(),
            Scheme::mono(Type::Fun(
                vec![],
                Box::new(Type::List(Box::new(Type::String))),
            )),
        );
    }

    fn register_fs_builtins(&mut self, env: &mut TypeEnv) {
        // fs.exists: (String) -> Bool
        env.define(
            "fs.exists".into(),
            Scheme::mono(Type::Fun(vec![Type::String], Box::new(Type::Bool))),
        );
    }

    fn register_test_builtins(&mut self, env: &mut TypeEnv) {
        // test.assert: (Bool, String) -> ()
        // The message parameter is optional at runtime; registering the full
        // arity lets the typechecker validate the message type while the
        // is_method_call arity tolerance still allows the 1-arg form.
        env.define(
            "test.assert".into(),
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
                "test.assert_eq".into(),
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
                "test.assert_ne".into(),
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
                env.define(name.to_string(), float_to_extfloat.clone());
            }
        }

        // Functions that always produce finite results: (Float) -> Float
        {
            let float_to_float = Scheme::mono(Type::Fun(vec![Type::Float], Box::new(Type::Float)));
            for name in &["math.sin", "math.cos", "math.tan", "math.atan"] {
                env.define(name.to_string(), float_to_float.clone());
            }
        }

        // math.pow: (Float, Float) -> ExtFloat (can overflow)
        {
            let ff_to_ef = Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::ExtFloat),
            ));
            env.define("math.pow".into(), ff_to_ef);
        }

        // math.atan2: (Float, Float) -> Float (always finite)
        {
            let ff_to_f = Scheme::mono(Type::Fun(
                vec![Type::Float, Type::Float],
                Box::new(Type::Float),
            ));
            env.define("math.atan2".into(), ff_to_f);
        }

        // Math constants
        env.define("math.pi".into(), Scheme::mono(Type::Float));
        env.define("math.e".into(), Scheme::mono(Type::Float));

        // Float constants
        env.define("float.max".into(), Scheme::mono(Type::Float));
        env.define("float.min".into(), Scheme::mono(Type::Float));
        env.define("float.epsilon".into(), Scheme::mono(Type::Float));
        env.define("float.min_positive".into(), Scheme::mono(Type::Float));
        env.define("float.infinity".into(), Scheme::mono(Type::ExtFloat));
        env.define("float.neg_infinity".into(), Scheme::mono(Type::ExtFloat));
        env.define("float.nan".into(), Scheme::mono(Type::ExtFloat));
    }

    fn register_channel_builtins(&mut self, env: &mut TypeEnv) {
        // channel.new: (Int) -> Channel  (opaque; use fresh var)
        {
            let (ch, chv) = self.fresh_tv();
            env.define(
                "channel.new".into(),
                Scheme {
                    vars: vec![chv],
                    ty: Type::Fun(vec![Type::Int], Box::new(ch)),
                    constraints: vec![],
                },
            );
        }

        // channel.send: (Channel, a) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "channel.send".into(),
                Scheme {
                    vars: vec![chv, av],
                    ty: Type::Fun(vec![ch, a], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // channel.receive: (Channel) -> ChannelResult(a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "channel.receive".into(),
                Scheme {
                    vars: vec![chv, av],
                    ty: Type::Fun(
                        vec![ch],
                        Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.close: (Channel) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            env.define(
                "channel.close".into(),
                Scheme {
                    vars: vec![chv],
                    ty: Type::Fun(vec![ch], Box::new(Type::Unit)),
                    constraints: vec![],
                },
            );
        }

        // channel.try_send: (Channel, a) -> Bool
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "channel.try_send".into(),
                Scheme {
                    vars: vec![chv, av],
                    ty: Type::Fun(vec![ch, a], Box::new(Type::Bool)),
                    constraints: vec![],
                },
            );
        }

        // channel.try_receive: (Channel) -> ChannelResult(a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "channel.try_receive".into(),
                Scheme {
                    vars: vec![chv, av],
                    ty: Type::Fun(
                        vec![ch],
                        Box::new(Type::Generic("ChannelResult".into(), vec![a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.select: (List(Channel)) -> (Channel, a)
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            env.define(
                "channel.select".into(),
                Scheme {
                    vars: vec![chv, av],
                    ty: Type::Fun(
                        vec![Type::List(Box::new(ch.clone()))],
                        Box::new(Type::Tuple(vec![ch, a])),
                    ),
                    constraints: vec![],
                },
            );
        }

        // channel.each: (Channel(a), Fn(a) -> b) -> Unit
        {
            let (ch, chv) = self.fresh_tv();
            let (a, av) = self.fresh_tv();
            let (b, bv) = self.fresh_tv();
            env.define(
                "channel.each".into(),
                Scheme {
                    vars: vec![chv, av, bv],
                    ty: Type::Fun(
                        vec![ch, Type::Fun(vec![a], Box::new(b))],
                        Box::new(Type::Unit),
                    ),
                    constraints: vec![],
                },
            );
        }
    }

    fn register_time_builtins(&mut self, env: &mut TypeEnv) {
        // ── Time module type definitions ──────────────────────────────

        let instant_ty = Type::Record("Instant".into(), vec![("epoch_ns".into(), Type::Int)]);
        let date_ty = Type::Record(
            "Date".into(),
            vec![
                ("year".into(), Type::Int),
                ("month".into(), Type::Int),
                ("day".into(), Type::Int),
            ],
        );
        let time_of_day_ty = Type::Record(
            "Time".into(),
            vec![
                ("hour".into(), Type::Int),
                ("minute".into(), Type::Int),
                ("second".into(), Type::Int),
                ("ns".into(), Type::Int),
            ],
        );
        let datetime_ty = Type::Record(
            "DateTime".into(),
            vec![
                ("date".into(), date_ty.clone()),
                ("time".into(), time_of_day_ty.clone()),
            ],
        );
        let duration_ty = Type::Record("Duration".into(), vec![("ns".into(), Type::Int)]);
        let weekday_ty = Type::Generic("Weekday".into(), vec![]);

        // Register record types so field access type-checks
        self.records.insert(
            "Instant".into(),
            RecordInfo {
                _name: "Instant".into(),
                _params: vec![],
                fields: vec![("epoch_ns".into(), Type::Int)],
            },
        );
        self.records.insert(
            "Date".into(),
            RecordInfo {
                _name: "Date".into(),
                _params: vec![],
                fields: vec![
                    ("year".into(), Type::Int),
                    ("month".into(), Type::Int),
                    ("day".into(), Type::Int),
                ],
            },
        );
        self.records.insert(
            "Time".into(),
            RecordInfo {
                _name: "Time".into(),
                _params: vec![],
                fields: vec![
                    ("hour".into(), Type::Int),
                    ("minute".into(), Type::Int),
                    ("second".into(), Type::Int),
                    ("ns".into(), Type::Int),
                ],
            },
        );
        self.records.insert(
            "DateTime".into(),
            RecordInfo {
                _name: "DateTime".into(),
                _params: vec![],
                fields: vec![
                    ("date".into(), date_ty.clone()),
                    ("time".into(), time_of_day_ty.clone()),
                ],
            },
        );
        self.records.insert(
            "Duration".into(),
            RecordInfo {
                _name: "Duration".into(),
                _params: vec![],
                fields: vec![("ns".into(), Type::Int)],
            },
        );

        // Register Weekday enum
        self.enums.insert(
            "Weekday".into(),
            EnumInfo {
                _name: "Weekday".into(),
                params: vec![],
                variants: vec![
                    VariantInfo {
                        name: "Monday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Tuesday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Wednesday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Thursday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Friday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Saturday".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "Sunday".into(),
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
            self.variant_to_enum
                .insert(day.to_string(), "Weekday".into());
            env.define(day.to_string(), Scheme::mono(weekday_ty.clone()));
        }

        // ── Function signatures ──────────────────────────────────────

        // time.now: () -> Instant
        env.define(
            "time.now".into(),
            Scheme::mono(Type::Fun(vec![], Box::new(instant_ty.clone()))),
        );

        // time.today: () -> Date
        env.define(
            "time.today".into(),
            Scheme::mono(Type::Fun(vec![], Box::new(date_ty.clone()))),
        );

        // time.date: (Int, Int, Int) -> Result(Date, String)
        env.define(
            "time.date".into(),
            Scheme::mono(Type::Fun(
                vec![Type::Int, Type::Int, Type::Int],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![date_ty.clone(), Type::String],
                )),
            )),
        );

        // time.time: (Int, Int, Int) -> Result(Time, String)
        env.define(
            "time.time".into(),
            Scheme::mono(Type::Fun(
                vec![Type::Int, Type::Int, Type::Int],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![time_of_day_ty.clone(), Type::String],
                )),
            )),
        );

        // time.datetime: (Date, Time) -> DateTime
        env.define(
            "time.datetime".into(),
            Scheme::mono(Type::Fun(
                vec![date_ty.clone(), time_of_day_ty.clone()],
                Box::new(datetime_ty.clone()),
            )),
        );

        // time.to_datetime: (Instant, Int) -> DateTime
        env.define(
            "time.to_datetime".into(),
            Scheme::mono(Type::Fun(
                vec![instant_ty.clone(), Type::Int],
                Box::new(datetime_ty.clone()),
            )),
        );

        // time.to_instant: (DateTime, Int) -> Instant
        env.define(
            "time.to_instant".into(),
            Scheme::mono(Type::Fun(
                vec![datetime_ty.clone(), Type::Int],
                Box::new(instant_ty.clone()),
            )),
        );

        // time.to_utc: (Instant) -> DateTime
        env.define(
            "time.to_utc".into(),
            Scheme::mono(Type::Fun(
                vec![instant_ty.clone()],
                Box::new(datetime_ty.clone()),
            )),
        );

        // time.from_utc: (DateTime) -> Instant
        env.define(
            "time.from_utc".into(),
            Scheme::mono(Type::Fun(
                vec![datetime_ty.clone()],
                Box::new(instant_ty.clone()),
            )),
        );

        // time.format: (DateTime, String) -> String
        env.define(
            "time.format".into(),
            Scheme::mono(Type::Fun(
                vec![datetime_ty.clone(), Type::String],
                Box::new(Type::String),
            )),
        );

        // time.format_date: (Date, String) -> String
        env.define(
            "time.format_date".into(),
            Scheme::mono(Type::Fun(
                vec![date_ty.clone(), Type::String],
                Box::new(Type::String),
            )),
        );

        // time.parse: (String, String) -> Result(DateTime, String)
        env.define(
            "time.parse".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![datetime_ty.clone(), Type::String],
                )),
            )),
        );

        // time.parse_date: (String, String) -> Result(Date, String)
        env.define(
            "time.parse_date".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String, Type::String],
                Box::new(Type::Generic(
                    "Result".into(),
                    vec![date_ty.clone(), Type::String],
                )),
            )),
        );

        // time.add_days: (Date, Int) -> Date
        env.define(
            "time.add_days".into(),
            Scheme::mono(Type::Fun(
                vec![date_ty.clone(), Type::Int],
                Box::new(date_ty.clone()),
            )),
        );

        // time.add_months: (Date, Int) -> Date
        env.define(
            "time.add_months".into(),
            Scheme::mono(Type::Fun(
                vec![date_ty.clone(), Type::Int],
                Box::new(date_ty.clone()),
            )),
        );

        // time.add: (Instant, Duration) -> Instant
        env.define(
            "time.add".into(),
            Scheme::mono(Type::Fun(
                vec![instant_ty.clone(), duration_ty.clone()],
                Box::new(instant_ty.clone()),
            )),
        );

        // time.since: (Instant, Instant) -> Duration
        env.define(
            "time.since".into(),
            Scheme::mono(Type::Fun(
                vec![instant_ty.clone(), instant_ty.clone()],
                Box::new(duration_ty.clone()),
            )),
        );

        // time.hours: (Int) -> Duration
        env.define(
            "time.hours".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
        );

        // time.minutes: (Int) -> Duration
        env.define(
            "time.minutes".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
        );

        // time.seconds: (Int) -> Duration
        env.define(
            "time.seconds".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
        );

        // time.ms: (Int) -> Duration
        env.define(
            "time.ms".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(duration_ty.clone()))),
        );

        // time.weekday: (Date) -> Weekday
        env.define(
            "time.weekday".into(),
            Scheme::mono(Type::Fun(vec![date_ty.clone()], Box::new(weekday_ty))),
        );

        // time.days_between: (Date, Date) -> Int
        env.define(
            "time.days_between".into(),
            Scheme::mono(Type::Fun(
                vec![date_ty.clone(), date_ty.clone()],
                Box::new(Type::Int),
            )),
        );

        // time.days_in_month: (Int, Int) -> Int
        env.define(
            "time.days_in_month".into(),
            Scheme::mono(Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int))),
        );

        // time.is_leap_year: (Int) -> Bool
        env.define(
            "time.is_leap_year".into(),
            Scheme::mono(Type::Fun(vec![Type::Int], Box::new(Type::Bool))),
        );

        // time.sleep: (Duration) -> Unit
        env.define(
            "time.sleep".into(),
            Scheme::mono(Type::Fun(vec![duration_ty], Box::new(Type::Unit))),
        );
    }

    fn register_http_builtins(&mut self, env: &mut TypeEnv) {
        // ── HTTP module type definitions ─────────────────────────────

        // Method enum
        let method_ty = Type::Generic("Method".into(), vec![]);

        self.enums.insert(
            "Method".into(),
            EnumInfo {
                _name: "Method".into(),
                params: vec![],
                variants: vec![
                    VariantInfo {
                        name: "GET".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "POST".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "PUT".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "PATCH".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "DELETE".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "HEAD".into(),
                        field_types: vec![],
                    },
                    VariantInfo {
                        name: "OPTIONS".into(),
                        field_types: vec![],
                    },
                ],
            },
        );
        for variant in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            self.variant_to_enum
                .insert(variant.to_string(), "Method".into());
            env.define(variant.to_string(), Scheme::mono(method_ty.clone()));
        }

        // Response record
        let map_ss = Type::Map(Box::new(Type::String), Box::new(Type::String));

        let response_ty = Type::Record(
            "Response".into(),
            vec![
                ("status".into(), Type::Int),
                ("body".into(), Type::String),
                ("headers".into(), map_ss.clone()),
            ],
        );

        self.records.insert(
            "Response".into(),
            RecordInfo {
                _name: "Response".into(),
                _params: vec![],
                fields: vec![
                    ("status".into(), Type::Int),
                    ("body".into(), Type::String),
                    ("headers".into(), map_ss.clone()),
                ],
            },
        );

        // Request record
        let request_ty = Type::Record(
            "Request".into(),
            vec![
                ("method".into(), method_ty.clone()),
                ("path".into(), Type::String),
                ("query".into(), Type::String),
                ("headers".into(), map_ss.clone()),
                ("body".into(), Type::String),
            ],
        );

        self.records.insert(
            "Request".into(),
            RecordInfo {
                _name: "Request".into(),
                _params: vec![],
                fields: vec![
                    ("method".into(), method_ty.clone()),
                    ("path".into(), Type::String),
                    ("query".into(), Type::String),
                    ("headers".into(), map_ss.clone()),
                    ("body".into(), Type::String),
                ],
            },
        );

        // ── Function signatures ──────────────────────────────────────

        let result_response =
            Type::Generic("Result".into(), vec![response_ty.clone(), Type::String]);

        // http.get: (String) -> Result(Response, String)
        env.define(
            "http.get".into(),
            Scheme::mono(Type::Fun(
                vec![Type::String],
                Box::new(result_response.clone()),
            )),
        );

        // http.request: (Method, String, String, Map(String, String)) -> Result(Response, String)
        env.define(
            "http.request".into(),
            Scheme::mono(Type::Fun(
                vec![method_ty, Type::String, Type::String, map_ss],
                Box::new(result_response),
            )),
        );

        // http.serve: (Int, Fn(Request) -> Response) -> Unit
        env.define(
            "http.serve".into(),
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
            "http.segments".into(),
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
        assert!(env.lookup("print").is_some(), "print not registered");
        assert!(env.lookup("println").is_some(), "println not registered");
        assert!(env.lookup("panic").is_some(), "panic not registered");
        assert!(env.lookup("Some").is_some(), "Some not registered");
        assert!(env.lookup("None").is_some(), "None not registered");
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
  http.get("http://example.com", #{})
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
