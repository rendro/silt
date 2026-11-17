use super::*;

impl TypeChecker {
    /// The signature of the generic function `f`, of the scheme
    /// `scheme`, as an editor's hover writes it (HK8): `fn ident(x: a) ->
    /// a`, `fn twice(xs: m(Int)) -> m(Int) where m: Functor`, a type
    /// constructor variable by the name its annotation gives it (else
    /// `m`, `n`, ...), a type variable by a letter; and the function's
    /// type with its variables so named. `None` for a function whose type
    /// holds no type variable of its own.
    pub(super) fn written_signature(&self, f: &FnDecl, scheme: &Scheme) -> Option<Signature> {
        let ty = self.apply(&scheme.ty);
        let Type::Fun(params, ret) = &ty else {
            return None;
        };
        // (Sets: a signature may have thousands of variables.)
        let quantified: Distinct<TyVar> = scheme.vars.iter().copied().collect();
        let free = free_vars_in(&ty);
        let generic = free.iter().any(|v| quantified.contains(v));
        if !generic || params.len() != f.params.len() {
            return None;
        }
        let heads: Distinct<TyVar> = (crate::types::constructor_heads(&ty).into_iter())
            .filter(|v| quantified.contains(v))
            .collect();
        // (A letter is no name of a parameter: `fn pair(a: c, b: d)`.)
        let mut taken: Vec<String> = (f.params.iter())
            .filter_map(|param| match (&param.kind, &param.pattern.kind) {
                (ParamKind::Data, PatternKind::Ident(name)) => Some(resolve(*name)),
                _ => None,
            })
            .collect();
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        let mut constructors = ["m", "n", "o", "p", "q", "r"].into_iter().map(String::from);
        let mut letters = ('a'..='z').map(|c| c.to_string());
        for v in free {
            if !quantified.contains(&v) {
                continue;
            }
            let written = (self.tables.constructor_names.get(&v))
                .map(|name| resolve(*name))
                .filter(|name| heads.contains(&v) && !taken.contains(name));
            let name = match written {
                Some(name) => Some(name),
                None if heads.contains(&v) => constructors.find(|n| !taken.contains(n)),
                None => letters.find(|n| !taken.contains(n)),
            }
            .unwrap_or_else(|| "_".to_string());
            taken.push(name.clone());
            let id = RigidId {
                var: v,
                name: intern(&name),
            };
            mapping.insert(v, Type::Rigid(id));
        }
        let named = |t: &Type| self.show_type(&substitute_vars(t, &mapping));
        let param_names: Vec<(String, bool)> = (f.params.iter())
            .map(|param| {
                let name = match &param.pattern.kind {
                    PatternKind::Ident(n) => resolve(*n),
                    _ => "_".to_string(),
                };
                (name, matches!(param.kind, ParamKind::Type))
            })
            .collect();
        let written_params: Vec<String> = (param_names.iter().zip(params))
            .map(|((name, is_type), ty)| match is_type {
                true => format!("type {name}"),
                false => format!("{name}: {}", named(ty)),
            })
            .collect();
        let mut text = format!("fn {}({})", f.name, written_params.join(", "));
        if !matches!(**ret, Type::Unit) {
            text.push_str(&format!(" -> {}", named(ret)));
        }
        // Each variable's bounds, by the variable's name, as `where`
        // writes them.
        let mut bounds: Vec<(String, Vec<String>)> = Vec::new();
        let mut parts: Vec<(String, Vec<(TraitKey, Vec<Type>)>)> = Vec::new();
        for pred in &scheme.preds {
            let Pred::Trait {
                tr, args, subject, ..
            } = pred
            else {
                continue;
            };
            let Type::Rigid(subject) = substitute_vars(&self.apply(subject), &mapping) else {
                continue;
            };
            let args: Vec<Type> = (args.iter())
                .map(|arg| substitute_vars(&self.apply(arg), &mapping))
                .collect();
            let bound = self.show_bound(*tr, &args);
            let subject = resolve(subject.name);
            let args: Vec<Type> = args.iter().map(|arg| self.apply(arg)).collect();
            match bounds.iter_mut().find(|(s, _)| *s == subject) {
                Some((_, traits)) if traits.contains(&bound) => {}
                Some((_, traits)) => {
                    traits.push(bound);
                    if let Some((_, written)) = parts.iter_mut().find(|(s, _)| *s == subject) {
                        written.push((*tr, args));
                    }
                }
                None => {
                    bounds.push((subject.clone(), vec![bound]));
                    parts.push((subject, vec![(*tr, args)]));
                }
            }
        }
        if !bounds.is_empty() {
            let bounds: Vec<String> = (bounds.iter())
                .map(|(subject, traits)| format!("{subject}: {}", traits.join(" + ")))
                .collect();
            text.push_str(&format!(" where {}", bounds.join(", ")));
        }
        Some(Signature {
            text,
            ty: substitute_vars(&ty, &mapping),
            parts: Box::new(SignatureParts {
                name: f.name,
                params: param_names,
                bounds: parts,
            }),
        })
    }

    // ── Error reporting ─────────────────────────────────────────────

    /// The two types of a message, rendered: two types of one name are
    /// told apart by their modules (`a.Pt` and `b.Pt`); the module's own
    /// type keeps its bare name.
    pub(super) fn show_apart(&self, a: &Type, b: &Type) -> (String, String) {
        let mut shown = self.show_types(&[a, b], false).into_iter();
        let a = shown.next().unwrap_or_default();
        (a, shown.next().unwrap_or_default())
    }

    /// The type of a message that names one type: a named type is
    /// qualified by its module when another type the session knows has
    /// its name (`other.Shape`, with `shapes.Shape` imported too).
    pub(super) fn show_type(&self, ty: &Type) -> String {
        self.show_types(&[ty], true).pop().unwrap_or_default()
    }

    /// The types of one message (see `show_apart`, `show_type`): a type
    /// of another module is written as a program writes it, with its
    /// module (`json.Value`, `time.Date`, `shapes.Shape`); a prelude
    /// type and the module's own keep their bare names, unless another
    /// type of the message, or with `session` another type the session
    /// knows, has the name (`prelude.Option`, `a.Pt` and `b.Pt`). In a
    /// REPL cell a type of an earlier cell declared again since, which
    /// the cell cannot name, is written with its cell (`<repl:1>.H`).
    fn show_types(&self, types: &[&Type], session: bool) -> Vec<String> {
        // (A type constructor variable not decided yet is written by its
        // name, `m(Int)`.)
        let named: Vec<Type> = types.iter().map(|ty| self.name_constructors(ty)).collect();
        let types: Vec<&Type> = named.iter().collect();
        let types = types.as_slice();
        Type::show_all(types, |r, clash| {
            (clash
                || (session && self.type_name_clashes(r))
                || self.is_other_module(r)
                || self.described_type(r).is_some()
                || self.shadowed_cell_type(r))
            .then(|| self.qualified_type(r))
        })
    }

    /// `ty` with each type constructor variable that is still a
    /// unification variable written by the name of the annotation's
    /// variable it is a use of, for a message.
    fn name_constructors(&self, ty: &Type) -> Type {
        let mut mapping: HashMap<TyVar, Type> = HashMap::new();
        collect_app_heads(ty, &mut |v| {
            if let Some(name) = self.tables.constructor_names.get(&v) {
                mapping.insert(
                    v,
                    Type::Rigid(RigidId {
                        var: v,
                        name: *name,
                    }),
                );
            }
        });
        match mapping.is_empty() {
            true => ty.clone(),
            false => substitute_vars(ty, &mapping),
        }
    }

    /// Whether `r` is, in a REPL cell, a type of an earlier cell that the
    /// cell does not name by its name: a later cell declared it again.
    fn shadowed_cell_type(&self, r: TypeRef) -> bool {
        self.is_cell
            && !self.named_types.contains(&r.id.0)
            && self
                .def(r.id.0)
                .is_some_and(|def| self.cells.contains(&def.module))
    }

    /// Whether `r` is a type of another module than the one being
    /// checked that a message writes with its module (by
    /// `written_member`, the one naming rule): a builtin module's
    /// (`json`, `time`) or a program module's. Not a prelude type, not
    /// one of the module's own, not, in a REPL cell, one of an earlier
    /// cell, which the cell writes by its name. (Only a test: the name
    /// written is `written_member`'s.)
    pub(super) fn is_other_module(&self, r: TypeRef) -> bool {
        match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, module)) => module.is_some(),
            None => self
                .def(r.id.0)
                .filter(|def| def.module != self.module)
                .and_then(|def| self.module_name_here(def.module))
                .is_some_and(|module| !resolve(module).starts_with('<')),
        }
    }

    /// The name this module knows the module `module` by in a message
    /// and in the editor: the name the file gives it where it imports it
    /// (`errs` for `import da as errs`; a module imported twice by the
    /// first import's name); else its own name, but a module of another
    /// package by the path an import here writes for it (`da`, the key of
    /// a dependency whose library it is, imported here or not), never by
    /// the name the module calls itself (`lib`); None for another
    /// package's module no import here can reach (its names are written
    /// bare, its types described: `described_type`).
    pub(super) fn module_name_here(&self, module: crate::defs::ModuleId) -> Option<Symbol> {
        if let Some(binding) = self.module_spelled.get(&module) {
            return Some(*binding);
        }
        let package = |m| self.tables.module_packages.get(&m);
        if package(module) != package(self.module) {
            return self.import_path(module);
        }
        self.tables.module_names.get(&module).copied()
    }

    /// The name `name` of the module `module` (a type, a trait, a
    /// function, a variant) as this file writes it, in every message,
    /// help and answer of the editor: bare where the file imports the
    /// name (`import da.{ Sq }`: `Sq`), with the file's name for the
    /// module where it has one (`errs.Sq`, `t.parse_map`), else with the
    /// module's own name (`da.Sq`), or described where that name is one
    /// the file gives something else (`Sq of module other`). A name of
    /// this module, or of the prelude, is bare. The one rule (NAM-1).
    pub(super) fn written_member(&self, module: crate::defs::ModuleId, name: &str) -> String {
        if module == self.module {
            return name.to_string();
        }
        if let Some(builtin) = module.builtin_name() {
            let by_item = self.imported_by_item(module, name).is_some();
            return written_builtin_member(
                builtin,
                name,
                by_item,
                self.module_spelled.get(&module).copied(),
            );
        }
        if let Some(spelled) = self.spelling_in_reach(module, name) {
            return spelled;
        }
        match self.module_written(module) {
            // (An earlier REPL cell's "module" has no name a program
            // writes: its names are in scope by name.)
            Some(ModuleWritten::Named(m)) if resolve(m).starts_with('<') => name.to_string(),
            Some(ModuleWritten::Named(m)) => format!("{m}.{name}"),
            Some(written @ ModuleWritten::Described { .. }) => written
                .type_name(intern(name))
                .replace(crate::types::AFTER_ARGUMENTS, ""),
            None => name.to_string(),
        }
    }

    /// The definition `id` as this file writes it ([`Self::written_member`]):
    /// a function of another module (`t.parse_map`, `parse_map`), or of
    /// this module by its name.
    pub(super) fn written_def(&self, id: crate::defs::DefId) -> Option<String> {
        let def = self.def(id)?;
        Some(self.written_member(def.module, &resolve(def.name)))
    }

    /// The builtin function `qualified` (`toml.parse_map`, as the registry
    /// names it) as this file writes it ([`Self::written_member`]).
    pub(super) fn written_builtin(&self, qualified: &str) -> String {
        match qualified.split_once('.') {
            Some((module, name)) => match crate::defs::ModuleId::builtin(module) {
                Some(id) => self.written_member(id, name),
                None => qualified.to_string(),
            },
            None => qualified.to_string(),
        }
    }

    /// The builtin function that `written` names as this file writes it
    /// ([`Self::written_builtin`] the other way): `csv.stringify` for
    /// `c.stringify` under `import csv as c`, for `stringify` after `import
    /// csv.{ stringify }`, and for `csv.stringify`.
    pub(super) fn builtin_of_written(&self, written: &str) -> Option<String> {
        let found = (crate::module::builtin_modules().iter()).find_map(|module| {
            let id = crate::defs::ModuleId::builtin(module)?;
            let name = match written.rsplit_once('.') {
                Some((_, name)) => name,
                None => written,
            };
            (self.written_member(id, name) == written).then(|| format!("{module}.{name}"))
        });
        found.or_else(|| Some(written.to_string()))
    }

    /// [`TypeChecker::module_name_here`] of the builtin module `module`
    /// (`json`): the name the file gives it where it imports it (`j` for
    /// `import json as j`), else its own.
    pub(super) fn builtin_module_name_here(&self, module: &str) -> Symbol {
        (crate::defs::ModuleId::builtin(module))
            .and_then(|id| self.module_spelled.get(&id).copied())
            .unwrap_or_else(|| intern(module))
    }

    /// How this module writes the module `module` of the program where a
    /// message, or the editor, names one of its types (see
    /// [`ModuleWritten`]): by the name `module_name_here` gives, or
    /// described where no import written here can reach it; a builtin
    /// module by `builtin_module_name_here`. None for this module itself
    /// and for the prelude.
    pub(super) fn module_written(&self, module: crate::defs::ModuleId) -> Option<ModuleWritten> {
        if module.is_builtin() {
            let name = module.builtin_name()?;
            return Some(ModuleWritten::Named(self.builtin_module_name_here(name)));
        }
        if module == self.module {
            return None;
        }
        let package = |m| self.tables.module_packages.get(&m);
        let described = |package: Option<Symbol>| {
            Some(ModuleWritten::Described {
                package,
                // (A package's library is named by its package: `InErr of
                // package inner`; the file's own package's `lib` by its
                // module, `T of module lib`.)
                module: (self.tables.module_names.get(&module).copied())
                    .filter(|name| package.is_none() || resolve(*name) != "lib"),
            })
        };
        if package(module) != package(self.module) && self.import_path(module).is_none() {
            return described(Some(*self.tables.package_names.get(package(module)?)?));
        }
        let name = self.module_name_here(module)?;
        // A module the file does not import, whose name the file gives
        // another module (`import da as other` beside a module `other`):
        // its types are described, so that two types are never written
        // alike (`other.T` is da's here).
        let renamed_away = !self.module_spelled.contains_key(&module) && self.bound_here(name);
        match renamed_away {
            true => described(None),
            false => Some(ModuleWritten::Named(name)),
        }
    }

    /// [`TypeChecker::module_written`] for every module this one reaches
    /// through its imports, the only modules whose types its code can
    /// hold: what the module's analysis keeps for the editor, so that
    /// hover writes another module's types as the messages do. (Not for
    /// every module the session knows: that made checking a program of n
    /// modules take time n squared.) For a REPL cell, which has no reach,
    /// every module of the program the session has.
    pub(super) fn modules_written(&self) -> HashMap<crate::defs::ModuleId, ModuleWritten> {
        let written =
            |module: &crate::defs::ModuleId| Some((*module, self.module_written(*module)?));
        // (The builtin modules the file names otherwise than by their own
        // names, `import json as j`: any other is written by its name.)
        let renamed = (self.module_spelled.iter())
            .filter(|(module, binding)| {
                (module.builtin_name()).is_some_and(|name| resolve(**binding) != name)
            })
            .filter_map(|(module, _)| written(module));
        let mut out: HashMap<_, _> = renamed.collect();
        match &self.reach {
            Some(reach) => out.extend(reach.get().iter().filter_map(written)),
            // A REPL cell (for `:type`) can name what the earlier cells
            // imported: every module of the program the session has.
            None => out.extend(
                (self.tables.module_names.keys())
                    .filter(|module| !self.cells.contains(module))
                    .filter_map(written),
            ),
        }
        out
    }

    /// The variant `name` of the enum `enum_ty` as a program writes it:
    /// with its module where the enum is another module's (`json.Bool`,
    /// `shapes.Circle`), bare otherwise (`Some`, a variant of the
    /// module's own).
    /// Where the module's imports put a name of another module in
    /// reach, as they spell it (`spelling_in_reach`); the module's own
    /// name where none does.
    pub(super) fn written_variant(&self, enum_ty: TypeRef, name: Symbol) -> String {
        match self.is_other_module(enum_ty) {
            // (Of a module no import here reaches, by its own name: the
            // module's name is none the program can write.)
            true if self.variant_out_of_reach(enum_ty, name) => name.to_string(),
            true => match self.module_of_type(enum_ty) {
                Some(id) => self.written_member(id, &resolve(name)),
                None => name.to_string(),
            },
            false => name.to_string(),
        }
    }

    /// Whether the variant `name` of `enum_ty`, a type of another module,
    /// is one this module cannot write in any form: no import here puts
    /// it in reach, and none written here can (a module of a dependency's
    /// dependency, a dependency's module other than its library).
    pub(super) fn variant_out_of_reach(&self, enum_ty: TypeRef, name: Symbol) -> bool {
        let package = |m| self.tables.module_packages.get(&m);
        self.def(enum_ty.id.0).is_some_and(|def| {
            !def.module.is_builtin()
                && package(def.module) != package(self.module)
                && self.spelling_in_reach(def.module, &resolve(name)).is_none()
                && self.import_path(def.module).is_none()
        })
    }

    /// The variant `name` of `enum_ty` as this module can write it, or
    /// None where its imports put it in no reach (`import errs.{
    /// AppError }` names no `Io`): for a help that offers code to write.
    pub(super) fn variant_in_reach(&self, enum_ty: TypeRef, name: Symbol) -> Option<String> {
        match self.is_other_module(enum_ty) {
            true => self.spelling_in_reach(self.module_of_type(enum_ty)?, &resolve(name)),
            false => Some(name.to_string()),
        }
    }

    /// The module that declares the type `r`: a builtin module's
    /// pseudo-module, or a module of the graph. None for the prelude.
    pub(super) fn module_of_type(&self, r: TypeRef) -> Option<crate::defs::ModuleId> {
        match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, module)) => crate::defs::ModuleId::builtin(module.as_ref()?),
            None => self.def(r.id.0).map(|def| def.module),
        }
    }

    /// How this module writes the name `name` of the module `module`:
    /// bare where an import item brings it in (`import m.{ name }`), by
    /// the name the module is bound to where it is imported (its alias,
    /// `e.Io`; `dep.Shown` for a dependency's root module, whatever that
    /// module calls itself); None where no import puts it in reach. The
    /// one rule for every help that writes a name of another module.
    pub(super) fn spelling_in_reach(
        &self,
        module: crate::defs::ModuleId,
        name: &str,
    ) -> Option<String> {
        if let Some(binding) = self.imported_by_item(module, name) {
            return Some(binding.to_string());
        }
        (self.module_spelled.get(&module)).map(|binding| format!("{binding}.{name}"))
    }

    /// The name the file binds the name `name` of the module `module` to,
    /// where it imports it by item (`import m.{ name }`).
    pub(super) fn imported_by_item(
        &self,
        module: crate::defs::ModuleId,
        name: &str,
    ) -> Option<Symbol> {
        (self.spelled.iter())
            .find(|(id, _)| {
                self.def(**id)
                    .is_some_and(|def| def.module == module && resolve(def.name) == name)
            })
            .map(|(_, binding)| *binding)
    }

    /// The names the module binds the builtin modules it imports to
    /// (`j` for `import json as j`; a module imported twice by its first
    /// import's name): what the session's own helps write a builtin's name
    /// with (`Session::written_builtin`).
    pub(super) fn builtin_bindings(&self) -> HashMap<crate::defs::ModuleId, Symbol> {
        (self.module_spelled.iter())
            .filter(|(module, _)| module.builtin_name().is_some())
            .map(|(module, binding)| (*module, *binding))
            .collect()
    }

    /// The path an import of `module` would write here, for a help that
    /// names the import to add: the path an import of the module uses
    /// already (`import dep.{ T }`), the key of a dependency of the
    /// package whose library it is, a builtin module's name, or the name
    /// of a module of the same package. None for any other module (a
    /// module of a dependency's dependency, a dependency's module other
    /// than its library: no import written here reaches it), and for a
    /// path the file binds already (`bound_here`): that import would
    /// bind it twice. The one rule of every "after `import X`" a help
    /// offers.
    pub(super) fn import_path(&self, module: crate::defs::ModuleId) -> Option<Symbol> {
        if let Some(path) = self.import_paths.get(&module) {
            return Some(*path);
        }
        self.path_to_import(module)
            .filter(|path| !self.bound_here(*path))
    }

    /// Whether the file binds the name `name` at its top level: to a
    /// module it imports (`import da as errs`), to an item it imports, or
    /// to a definition of its own.
    pub(super) fn bound_here(&self, name: Symbol) -> bool {
        (self.module_spelled.values().chain(self.spelled.values())).any(|b| *b == name)
            || (self.defs.as_ref()).is_some_and(|defs| !defs.named(self.module, name).is_empty())
    }

    /// The path of `import_path` before the file's own names are
    /// looked at.
    fn path_to_import(&self, module: crate::defs::ModuleId) -> Option<Symbol> {
        // (A dependency's library that another module of the package
        // imports: by its key.)
        if let Some(key) = self.dependency_keys.get(&module) {
            return Some(*key);
        }
        if let Some(name) = module.builtin_name() {
            return Some(intern(name));
        }
        let package = |m| self.tables.module_packages.get(&m);
        (package(module).is_some() && package(module) == package(self.module))
            .then(|| self.tables.module_names.get(&module).copied())
            .flatten()
            .filter(|name| !resolve(*name).starts_with('<'))
    }

    /// Whether another type the session knows has the name of `r`.
    fn type_name_clashes(&self, r: TypeRef) -> bool {
        let other = |o: &TypeRef| o.name == r.name && o.id != r.id;
        self.tables.enums.keys().any(other)
            || self.tables.records.keys().any(other)
            || crate::defs::builtin_type_id(&resolve(r.name)).is_some_and(|id| id != r.id)
    }

    /// The name of `r` written with its module: `a.Pt`, `time.Weekday`,
    /// `prelude.Option` for a prelude type; a module's own type keeps its
    /// bare name, and so in a REPL cell does a type of an earlier cell
    /// that the cell names by its name (`<repl:1>.H` is the `H` an
    /// earlier cell declared again).
    fn qualified_type(&self, r: TypeRef) -> String {
        match crate::defs::builtin_types().get(r.id.0.0 as usize) {
            Some((_, Some(module))) => match crate::defs::ModuleId::builtin(module) {
                Some(id) => self.written_member(id, &resolve(r.name)),
                None => format!("{module}.{}", r.name),
            },
            Some((_, None)) => format!("prelude.{}", r.name),
            None => match self.def(r.id.0) {
                Some(def)
                    if def.module != self.module
                        && !(self.is_cell
                            && self.cells.contains(&def.module)
                            && self.named_types.contains(&r.id.0)) =>
                {
                    match self.spelling_in_reach(def.module, &resolve(r.name)) {
                        Some(spelled) => spelled,
                        None => match self.module_written(def.module) {
                            Some(ModuleWritten::Named(module)) => format!("{module}.{}", r.name),
                            Some(written @ ModuleWritten::Described { .. }) => {
                                written.type_name(r.name)
                            }
                            None => r.name.to_string(),
                        },
                    }
                }
                _ => r.name.to_string(),
            },
        }
    }

    /// The type `r` of a module of another package that no import
    /// written here can reach (a dependency's dependency, a dependency's
    /// module other than its library), in the one form a message writes
    /// such a type in, which is no code: `InErr of package inner`, `UErr
    /// of module util of package da` (the arguments of a generic one
    /// before the description, see [`crate::types::AFTER_ARGUMENTS`]).
    /// None for any other type.
    fn described_type(&self, r: TypeRef) -> Option<String> {
        let def = self.def(r.id.0)?;
        match self.module_written(def.module)? {
            written @ ModuleWritten::Described { .. } => Some(written.type_name(r.name)),
            ModuleWritten::Named(_) => None,
        }
    }

    /// The name of `r` as the module being checked writes it, for a
    /// hint that spells code: a type of another module with the module
    /// (`time.Date`); in a REPL cell the types of the earlier cells are
    /// in scope by name.
    pub(super) fn written_type(&self, r: TypeRef) -> String {
        let qualified = self
            .qualified_type(r)
            .replace(crate::types::AFTER_ARGUMENTS, "");
        // (An earlier cell's "module" has no name a program writes.)
        match self.is_cell && qualified.starts_with('<') {
            true => r.name.to_string(),
            false => qualified,
        }
    }

    /// The name of the trait `t` in a message, as a program writes it:
    /// a trait of another module with its module (`json.Encode`,
    /// `a.Show`); a prelude trait and the module's own bare (`Display`),
    /// unless another trait the session knows has its name
    /// (`prelude.Display` beside a program's `Display`), or in a REPL
    /// cell, of an earlier cell's, unless the cell names it by its name
    /// (`<repl:1>.Greet` is the `Greet` a later cell declared again).
    pub(super) fn show_trait(&self, t: TraitKey) -> String {
        let of_another_module = match crate::defs::builtin_trait_of_id(t.id.0) {
            Some((_, module)) => module.is_some(),
            None => self
                .def(t.id.0)
                .filter(|def| def.module != self.module)
                .and_then(|def| self.tables.module_names.get(&def.module))
                .is_some_and(|module| !resolve(*module).starts_with('<')),
        };
        let clashes = self
            .tables
            .traits
            .keys()
            .any(|o| o.name == t.name && o.id != t.id);
        let named_by_cell = self.is_cell
            && self.named_types.contains(&t.id.0)
            && (self.def(t.id.0)).is_some_and(|def| self.cells.contains(&def.module));
        match of_another_module || (clashes && !named_by_cell) {
            true => self.show_trait_in_module(t),
            false => t.name.to_string(),
        }
    }

    /// The name of the trait `t` as the module being checked writes it:
    /// with its module where another module declares it (`m.Show`,
    /// `json.Encode`), a prelude trait by its name (`Display`); in a
    /// REPL cell the traits of the earlier cells are in scope by name.
    pub(super) fn written_trait(&self, t: TraitKey) -> String {
        if let Some((name, None)) = crate::defs::builtin_trait_of_id(t.id.0) {
            return name.to_string();
        }
        let qualified = self.show_trait_in_module(t);
        // (An earlier cell's "module" has no name a program writes.)
        match self.is_cell && qualified.starts_with('<') {
            true => t.name.to_string(),
            false => qualified,
        }
    }

    /// The trait with the module that declares it, where that is not
    /// the module being checked: `a.Show`, `prelude.Display`.
    pub(super) fn show_trait_in_module(&self, t: TraitKey) -> String {
        if let Some((_, module)) = crate::defs::builtin_trait_of_id(t.id.0) {
            return match module.and_then(crate::defs::ModuleId::builtin) {
                Some(id) => self.written_member(id, &resolve(t.name)),
                None => format!("prelude.{}", t.name),
            };
        }
        match self.def(t.id.0) {
            Some(def) if def.module != self.module => {
                self.written_member(def.module, &resolve(t.name))
            }
            _ => t.name.to_string(),
        }
    }

    /// The quick fix for a value where a `Result` is expected: wrap the
    /// expression in `Ok(...)`.
    pub(super) fn add_ok_wrap_fix(d: &mut Diagnostic, got: &Type, expected: &Type) {
        let is_result = |t: &Type| matches!(t, Type::Generic(n, _) if n.is_builtin("Result"));
        if is_result(expected) && !is_result(got) && d.span.is_in_source() {
            let (start, end) = (
                Span::point(d.span.file, d.span.start),
                Span::point(d.span.file, d.span.end),
            );
            d.fixes.push(crate::diagnostic::Fix {
                title: "Wrap expression in `Ok(...)`".to_string(),
                edits: vec![(start, "Ok(".to_string()), (end, ")".to_string())],
            });
        }
    }

    /// If `got` is a `Result(_, _)` or `Option(_)` but `expected` is
    /// not, the help line that explains how to thread the value
    /// through: the raw "expected String, got Result(String, _)" is
    /// correct but doesn't say how to fix it; the help points at `?` and
    /// `result.flat_map` / `option.flat_map`. A `String` where a
    /// `regex.Regex` is expected (a pattern given as text) is told to
    /// compile it.
    pub(super) fn chain_hint(&self, got: &Type, expected: &Type) -> Option<std::string::String> {
        let is_wrapper = |t: &Type, name: &str| -> bool { t.is_builtin(name) };
        // (A type constructor, `Option(_)`, holds no value to chain.)
        if matches!(got, Type::Con(..)) {
            return None;
        }
        // (A pattern written where a compiled one is taken.)
        if matches!(got, Type::String) && expected.is_builtin("Regex") {
            return Some(format!(
                "a regex function takes a compiled pattern: make one with `{}(pattern)`, once, \
                 and pass it on",
                self.written_builtin("regex.compile")
            ));
        }
        if is_wrapper(expected, "Result") || is_wrapper(expected, "Option") {
            return None;
        }
        if is_wrapper(got, "Result") {
            return Some(format!(
                "to chain through a `Result`, use `?` to propagate the \
                 error, or `|> {} {{ x -> ... }}` to continue the \
                 pipeline on the Ok value",
                self.written_builtin("result.flat_map")
            ));
        }
        if is_wrapper(got, "Option") {
            return Some(format!(
                "to chain through an `Option`, use `?` to propagate \
                 `None`, or `|> {} {{ x -> ... }}` to continue the \
                 pipeline on the Some value",
                self.written_builtin("option.flat_map")
            ));
        }
        None
    }

    pub(super) fn error(
        &mut self,
        code: Code,
        message: impl Into<std::string::String>,
        span: Span,
    ) {
        self.errors.push(Diagnostic::error(code, span, message));
    }

    /// An error with a message and, when there is one, a help line: what
    /// the `*_message` helpers that suggest a close name return.
    pub(super) fn error_help(
        &mut self,
        code: Code,
        (message, help): (std::string::String, Option<std::string::String>),
        span: Span,
    ) {
        let mut d = Diagnostic::error(code, span, message);
        d.help.extend(help);
        self.errors.push(d);
    }

    /// The known type that `name`, a name in a type annotation, spells
    /// in the wrong case: `int` → `Int`, `INT` → `Int`, `option` →
    /// `Option`. Type names are case-sensitive and declarations must be
    /// capitalised, so such a name can only be a typo for that type.
    /// Unless `name` is applied to arguments (`q(Int)`), one-letter
    /// names are left alone: `a`, `e`, `t` are the usual type variables,
    /// whatever types a program declares.
    pub(super) fn case_mismatched_type_name(&self, name: &str, applied: bool) -> Option<String> {
        if !applied && name.chars().count() < 2 {
            return None;
        }
        let matches = |candidate: &str| candidate != name && candidate.eq_ignore_ascii_case(name);
        crate::types::builtins::BUILTIN_TYPES
            .iter()
            .map(|t| t.name.to_string())
            .chain(self.tables.records.keys().map(|t| resolve(t.name)))
            .chain(self.tables.enums.keys().map(|t| resolve(t.name)))
            .chain(self.tables.type_aliases.iter().map(|t| resolve(t.name)))
            .filter(|candidate| matches(candidate))
            .min()
    }

    /// The "unknown type" error for `name` (resolved to `res`), with a
    /// hint when it is a known type in the wrong case (see
    /// `case_mismatched_type_name`), else what the name is where the
    /// module sees it as something else (`what_the_name_is`).
    pub(super) fn unknown_type_message(
        &self,
        name: &str,
        applied: bool,
        res: Option<crate::defs::Res>,
    ) -> String {
        match (self.case_mismatched_type_name(name, applied)).and_then(|t| self.type_offered(&t)) {
            Some(type_name) => format!(
                "unknown type '{name}' — did you mean {type_name}? (type names are \
                 case-sensitive and start with a capital letter; a lowercase name in a \
                 type is a type variable)"
            ),
            None => match self.what_the_name_is(res, intern(name), false) {
                Some(is) => format!("unknown type '{name}': {is}"),
                None => format!("unknown type '{name}'"),
            },
        }
    }

    /// What the name `name`, resolved to `res`, is where a type is
    /// wanted (`want_trait` false) or a trait (true) and it is the other
    /// thing, or a variant the module names bare: "`Show` is a trait",
    /// "`Own` is a type", "`Mine` is a variant of `Own`" (the enum as the
    /// module writes it). None where it is none of them.
    pub(super) fn what_the_name_is(
        &self,
        res: Option<crate::defs::Res>,
        name: Symbol,
        want_trait: bool,
    ) -> Option<String> {
        use crate::defs::DefKind;
        if let Some(crate::defs::Res::Def(id)) = res
            && let Some(def) = self.def(id)
        {
            return match def.kind {
                DefKind::Trait(_) if !want_trait => Some(format!("`{name}` is a trait")),
                DefKind::Type(_) | DefKind::TypeAlias if want_trait => {
                    Some(format!("`{name}` is a type"))
                }
                _ => None,
            };
        }
        let id = *self.bare_variants.get(&name)?;
        let ty = TypeRef {
            id,
            name: self.def(id.0)?.name,
        };
        let enum_name = self.show_type(&Type::Generic(ty, Vec::new()));
        Some(format!("`{name}` is a variant of `{enum_name}`"))
    }

    /// The type named `name` as this module can write it, for a "did
    /// you mean": bare for the module's own and the prelude's, as an
    /// import here puts it in reach (`regex.Regex` under `import regex`),
    /// else with the import to add (`regex.Regex` (a type of module
    /// `regex`, after `import regex`)). Of several types of that name the
    /// first of those, in that order. None where no import written here
    /// reaches any of them.
    fn type_offered(&self, name: &str) -> Option<String> {
        let builtin = crate::defs::builtin_type_id(name).map(|id| TypeRef {
            id,
            name: intern(name),
        });
        let declared = (self.tables.records.keys())
            .chain(self.tables.enums.keys())
            .chain(self.tables.type_aliases.iter())
            .filter(|r| resolve(r.name) == name)
            .copied();
        builtin
            .into_iter()
            .chain(declared)
            .filter_map(|r| match self.module_of_type(r) {
                None => Some((0, format!("`{name}`"))),
                Some(module) if module == self.module => Some((0, format!("`{name}`"))),
                Some(module) => match self.spelling_in_reach(module, name) {
                    Some(written) => Some((1, format!("`{written}`"))),
                    None => self.import_path(module).map(|path| {
                        (
                            2,
                            format!(
                                "`{path}.{name}` (a type of module `{path}`, after `import {path}`)"
                            ),
                        )
                    }),
                },
            })
            .min()
            .map(|(_, shown)| shown)
    }
}

/// Call `f` with each unification variable of `ty` (those of type
/// constructor variables have names).
fn collect_app_heads(ty: &Type, f: &mut impl FnMut(TyVar)) {
    match ty {
        Type::Var(v) => f(*v),
        Type::App(head, arg) => {
            collect_app_heads(head, f);
            collect_app_heads(arg, f);
        }
        Type::Fun(params, ret) => {
            params.iter().for_each(|t| collect_app_heads(t, f));
            collect_app_heads(ret, f);
        }
        Type::List(t) | Type::Set(t) | Type::Channel(t) => collect_app_heads(t, f),
        Type::Map(k, v) => {
            collect_app_heads(k, f);
            collect_app_heads(v, f);
        }
        Type::Tuple(ts) | Type::Generic(_, ts) | Type::Con(_, ts) => {
            ts.iter().for_each(|t| collect_app_heads(t, f))
        }
        _ => {}
    }
}

/// The name `name` of the builtin module `module` as a file writes it: bare
/// where the file imports it by item, with the file's name for the module
/// where it binds one (`binding`: `j` for `import json as j`), else with
/// the module's own name. The rule of [`TypeChecker::written_member`] for a
/// builtin module, which the session's helps use too.
pub(crate) fn written_builtin_member(
    module: &str,
    name: &str,
    by_item: bool,
    binding: Option<Symbol>,
) -> String {
    match (by_item, binding) {
        (true, _) => name.to_string(),
        (false, Some(binding)) => format!("{binding}.{name}"),
        (false, None) => format!("{module}.{name}"),
    }
}

/// How a module writes another module of the program where it names one
/// of its types: the one rule of the checker's messages and of the
/// editor's hover, signature help, completion details and inlay hints
/// (`TypeChecker::module_written`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModuleWritten {
    /// By a name a program writes: its own (`shapes`), a dependency's
    /// key for the dependency's library (`da`, imported here or not).
    Named(Symbol),
    /// Out of every import's reach (a dependency's dependency, a
    /// dependency's module other than its library), or a module of the
    /// package whose name the file gives another module: described.
    Described {
        /// The module's package; None for the file's own.
        package: Option<Symbol>,
        /// The module's name; None for a package's library.
        module: Option<Symbol>,
    },
}

impl ModuleWritten {
    /// The type `name` of such a module: `da.AppError`; `InErr of
    /// package inner`, `UErr of module util of package da`, `T of module
    /// other` (the arguments of a generic one stand before the
    /// description, at [`crate::types::AFTER_ARGUMENTS`]).
    pub(crate) fn type_name(&self, name: Symbol) -> String {
        let after = crate::types::AFTER_ARGUMENTS;
        match self {
            ModuleWritten::Named(module) => format!("{module}.{name}"),
            ModuleWritten::Described { package, module } => {
                let module = module.map(|m| format!(" of module {m}"));
                let package = package.map(|p| format!(" of package {p}"));
                format!(
                    "{name}{after}{}{}",
                    module.unwrap_or_default(),
                    package.unwrap_or_default()
                )
            }
        }
    }
}
