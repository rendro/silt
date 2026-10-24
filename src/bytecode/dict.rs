//! Dictionaries: how a value of a type that has a trait a program
//! declares reaches the trait's methods (`crate::evidence`). The checker
//! names, for each use of a definition with bounds and each call of a
//! trait's method, the evidence that the types have the traits; the
//! compiler turns it into dictionaries the code passes on, and a call of
//! a method takes the method out of the dictionary the code has.
//!
//! A dictionary is a [`Value::Dict`]: the [`Template`] of an impl (its
//! methods' global slots, in the order the trait declares them) and the
//! dictionaries of the predicates the impl's header asks. No program
//! value is one: dictionaries are only in the code's hidden parameters,
//! upvalues and other dictionaries.

use std::sync::{Arc, OnceLock};

use super::Globals;
use crate::defs::{TraitId, TypeId};
use crate::value::Value;

/// What every dictionary of one impl shares: the impl's trait and type,
/// its methods, and how to make the dictionaries of the trait's
/// supertraits and of the `where` clauses on the trait's parameters from
/// the dictionaries of its header.
#[derive(Debug)]
pub struct Template {
    /// Its place in the program's table of templates (`Globals`).
    pub index: u32,
    pub tr: TraitId,
    pub ty: TypeId,
    /// The impl, as a message names it: `Greet for Box`.
    pub name: String,
    /// The global slot of each method of the trait, in the order the
    /// trait declares them: the impl's own, or the trait's default.
    pub methods: Box<[u16]>,
    /// The dictionary of each supertrait of the trait, in the order the
    /// trait declares them.
    pub supers: Box<[Recipe]>,
    /// The dictionary of each `where` clause the trait declares on its
    /// parameters, in order.
    pub wheres: Box<[Recipe]>,
}

/// How a dictionary is made from the dictionaries of an impl's header.
#[derive(Debug, Clone)]
pub enum Recipe {
    /// A dictionary of the impl of `template`, of the dictionaries the
    /// recipes make.
    Dict {
        template: Arc<Template>,
        subs: Box<[Recipe]>,
    },
    /// The `k`th dictionary of the header.
    Header(u16),
    /// The `index`th supertrait's of what the recipe makes.
    Super(Box<Recipe>, u16),
    /// A trait found by the value (a builtin one): no dictionary.
    ByValue,
}

/// A dictionary.
#[derive(Debug, Clone)]
pub enum Dict {
    /// The dictionary of an impl whose header asks nothing (an impl for
    /// a type with no parameters): the place of its template in the
    /// program's table. Passing it on costs nothing.
    Of(u32),
    /// The dictionary of an impl with the dictionaries of its header.
    Built(Arc<Built>),
}

/// The dictionary of an impl's template with the dictionaries its
/// header's predicates have here.
#[derive(Debug)]
pub struct Built {
    pub template: Arc<Template>,
    subs: Subs,
    /// The supertraits' dictionaries, made when first asked for.
    supers: Box<[OnceLock<Value>]>,
    wheres: Box<[OnceLock<Value>]>,
}

/// The dictionaries of an impl's header: one, the most common (an impl
/// for `Box(a)`, `List(a)`), is kept in the dictionary itself, which is
/// then one allocation.
#[derive(Debug)]
enum Subs {
    One(Value),
    Many(Box<[Value]>),
}

impl Subs {
    fn as_slice(&self) -> &[Value] {
        match self {
            Subs::One(sub) => std::slice::from_ref(sub),
            Subs::Many(subs) => subs,
        }
    }
}

impl Dict {
    /// The dictionary of `template` with the header's dictionaries
    /// `subs`.
    pub fn new(template: &Arc<Template>, subs: impl ExactSizeIterator<Item = Value>) -> Dict {
        let subs = match subs.len() {
            0 => return Dict::Of(template.index),
            1 => Subs::One(subs.into_iter().next().unwrap_or(Value::Unit)),
            _ => Subs::Many(subs.collect()),
        };
        let supers = (0..template.supers.len())
            .map(|_| OnceLock::new())
            .collect();
        let wheres = (0..template.wheres.len())
            .map(|_| OnceLock::new())
            .collect();
        Dict::Built(Arc::new(Built {
            template: template.clone(),
            subs,
            supers,
            wheres,
        }))
    }

    /// The place of the dictionary's template in the program's table.
    pub fn index(&self) -> u32 {
        match self {
            Dict::Of(index) => *index,
            Dict::Built(built) => built.template.index,
        }
    }

    /// The dictionary's template, in the program's table `globals`.
    pub fn template<'a>(&'a self, globals: &'a Globals) -> Option<&'a Arc<Template>> {
        match self {
            Dict::Of(index) => globals.template_at(*index),
            Dict::Built(built) => Some(&built.template),
        }
    }

    /// The dictionary of the `k`th predicate of the impl's header.
    pub fn sub(&self, k: usize) -> Value {
        match self {
            Dict::Of(_) => Value::Unit,
            Dict::Built(built) => (built.subs.as_slice().get(k).cloned()).unwrap_or(Value::Unit),
        }
    }

    /// The dictionary of the `index`th supertrait of the trait.
    pub fn super_dict(&self, index: usize, globals: &Globals) -> Value {
        self.part(index, globals, |t| &t.supers, |b| &b.supers)
    }

    /// The dictionary of the `index`th `where` clause on the trait's
    /// parameters.
    pub fn where_dict(&self, index: usize, globals: &Globals) -> Value {
        self.part(index, globals, |t| &t.wheres, |b| &b.wheres)
    }

    /// `super_dict`, `where_dict`: the dictionary the `index`th of the
    /// template's recipes `recipes` makes (and keeps, in `cells`).
    fn part(
        &self,
        index: usize,
        globals: &Globals,
        recipes: impl Fn(&Template) -> &[Recipe],
        cells: impl Fn(&Built) -> &[OnceLock<Value>],
    ) -> Value {
        let Some(template) = self.template(globals) else {
            return Value::Unit;
        };
        let Some(recipe) = recipes(template).get(index) else {
            return Value::Unit;
        };
        match self {
            Dict::Of(_) => make(recipe, &[], globals),
            Dict::Built(built) => match cells(built).get(index) {
                Some(cell) => {
                    let subs = built.subs.as_slice();
                    (cell.get_or_init(|| make(recipe, subs, globals))).clone()
                }
                None => Value::Unit,
            },
        }
    }
}

/// The dictionary `recipe` makes from the dictionaries `subs` of a
/// header.
fn make(recipe: &Recipe, subs: &[Value], globals: &Globals) -> Value {
    match recipe {
        Recipe::Dict {
            template,
            subs: parts,
        } => Value::Dict(Dict::new(
            template,
            parts.iter().map(|part| make(part, subs, globals)),
        )),
        Recipe::Header(k) => (subs.get(usize::from(*k)).cloned()).unwrap_or(Value::Unit),
        Recipe::Super(of, index) => match make(of, subs, globals) {
            Value::Dict(dict) => dict.super_dict(usize::from(*index), globals),
            _ => Value::Unit,
        },
        Recipe::ByValue => Value::Unit,
    }
}
