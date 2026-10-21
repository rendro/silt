//! The parameters of a statement and the rows of its result.
//!
//! A parameter goes to the server as text, which the server parses as
//! the type the statement needs at its place (`$1` compared with an
//! `int4` column is an `int4`): an `Int` goes into an `int4` or a
//! `numeric`, a `String` into a `date`, a `Float` into a `float4`.
//!
//! A row is decoded into the type the program names, a record type or
//! `json.Value`. The record's fields are matched with the result's
//! columns by name once per result ([`Plan::new`]): a field with no
//! column, or one whose type the column's type cannot fill, is an error
//! before any row is read, however the values would have turned out (an
//! `Int` field is not filled from a `float8` column even where it holds
//! `2.0`). A column no field names is not read. A `json` or `jsonb`
//! column is decoded into the field's type by the typed decoder, through
//! json's rules.

use std::fmt::Write as _;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use fallible_iterator::FallibleIterator;
use postgres_protocol::types as wire;
use tokio_postgres::types::{Format, FromSql, IsNull, Kind, ToSql, Type as PgType};

use crate::builtins::data::{self, Data};
use crate::builtins::registry::decoding::Family;
use crate::builtins::time::{make_date, make_datetime, make_time};
use crate::builtins::typed::Type;
use crate::builtins::{decode, json};
use crate::typeinfo::{FieldType, Shape, TypeInfo, bv};
use crate::value::{Float, Value};
use crate::vm::{Runtime, VmError};

// ── Parameters ──────────────────────────────────────────────────────

/// A parameter as the server gets it: its text, or NULL.
#[derive(Debug)]
pub(super) enum TextParam {
    Null,
    Text(String),
    /// A list: an array where the statement's parameter is an array
    /// (`id = ANY($1)`), JSON anywhere else. A list of `Bytes` has no
    /// JSON form, and is an error only where JSON is what it is to be:
    /// the encoder leaves the error in `unsendable` before the statement
    /// is sent.
    List {
        array: String,
        json: Result<String, String>,
        unsendable: Unsendable,
    },
}

/// Where the encoder leaves why it could not send a parameter: the
/// error of the program, worded as `json.stringify`'s, that the task
/// gets instead of the statement's result.
#[derive(Clone, Debug, Default)]
pub(super) struct Unsendable(Arc<std::sync::Mutex<Option<String>>>);

impl Unsendable {
    /// Why a parameter could not be sent, if one could not.
    pub(super) fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn found(&self, why: &str) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
    }
}

impl ToSql for TextParam {
    fn to_sql(
        &self,
        ty: &PgType,
        out: &mut tokio_postgres::types::private::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        let text = match self {
            TextParam::Null => return Ok(IsNull::Yes),
            TextParam::Text(text) => text,
            TextParam::List {
                array,
                json,
                unsendable,
            } => match (ty.kind(), json) {
                (Kind::Array(_), _) => array,
                (_, Ok(json)) => json,
                (_, Err(why)) => {
                    unsendable.found(why);
                    return Err(why.clone().into());
                }
            },
        };
        out.extend_from_slice(text.as_bytes());
        Ok(IsNull::No)
    }

    fn accepts(_ty: &PgType) -> bool {
        true
    }

    fn encode_format(&self, _ty: &PgType) -> Format {
        Format::Text
    }

    tokio_postgres::types::to_sql_checked!();
}

/// The parameters of a statement of `caller`: `params`, a tuple, or
/// `()` for none. (The checker refuses parameters that are no tuple
/// wherever a program gives them: the error is a guard.)
pub(super) fn param_values(caller: &str, params: &Value) -> Result<Vec<Value>, VmError> {
    match params {
        Value::Unit => Ok(Vec::new()),
        Value::Tuple(values) => Ok(values.to_vec()),
        other => Err(VmError::new(format!(
            "{caller}: params must be a tuple, got {}; write (id,)",
            other.kind()
        ))),
    }
}

/// The parameters of a statement of `caller`, each written as data
/// (`Vm::encode`, which writes a part of a type with a written
/// `json.Encode` impl as the impl says); and where the encoder leaves
/// why one of them could not be sent, if one of them may not be (a list
/// of `Bytes`, which has no JSON form).
pub(super) fn params(
    caller: &str,
    params: Vec<Data>,
) -> Result<(Vec<TextParam>, Option<Unsendable>), VmError> {
    let unsendable = Unsendable::default();
    let params = params
        .into_iter()
        .map(|value| param(caller, value, &unsendable));
    let params: Vec<TextParam> = params.collect::<Result<_, _>>()?;
    let may_fail = params
        .iter()
        .any(|param| matches!(param, TextParam::List { json: Err(_), .. }));
    Ok((params, may_fail.then_some(unsendable)))
}

/// The parameter `value`. `None` is NULL and `Some(x)` is `x`; a date,
/// a time and a datetime are written as silt shows them (a datetime
/// with the offset `+00`, a year before 1 as a year BC), `Bytes` as
/// `bytea`'s hex text; a variant without fields is its name, as a
/// string is; a list is an array or JSON (see [`TextParam`]), and every
/// other value that is no scalar (a record, a map, a tuple, a variant
/// with fields, a `json.Value`) is written as `json.stringify` writes
/// it. (A value JSON has no form for is the error `json.stringify`
/// gives: the writer's, in place of the data.)
fn param(caller: &str, value: Data, unsendable: &Unsendable) -> Result<TextParam, VmError> {
    Ok(match value {
        Data::Null => TextParam::Null,
        Data::Array(_) => TextParam::List {
            array: array_literal(caller, &value)?,
            json: json::text(caller, &value, false).map_err(|e| e.message),
            unsendable: unsendable.clone(),
        },
        _ => TextParam::Text(scalar_text(caller, &value)?),
    })
}

/// The text of `value`, which is no list and no null: a scalar's own,
/// JSON for the others.
fn scalar_text(caller: &str, value: &Data) -> Result<String, VmError> {
    Ok(match value {
        Data::Int(n) => n.to_string(),
        // (`2.0` stays `2.0`, which no integer column takes.)
        Data::Float(f) => format!("{f:?}"),
        Data::Bool(b) => b.to_string(),
        Data::String(s) => s.to_string(),
        Data::Bytes(bytes) => hex(bytes),
        Data::Date(date) => {
            let (date, era) = date_text(date);
            format!("{date}{era}")
        }
        Data::Time(time) => time.to_string(),
        // In UTC, as a `timestamptz` is read: a `timestamp` ignores
        // the offset.
        Data::DateTime(at) => {
            let (date, time) = &**at;
            let (date, era) = date_text(date);
            format!("{date}T{time}+00{era}")
        }
        _ => json::text(caller, value, false)?,
    })
}

/// The text of the date `date` as Postgres reads it, and its era: a
/// year before 1 is a year BC (year 0 is 1 BC), `0044-03-15` and
/// `" BC"` for -43.
fn date_text(date: &data::Date) -> (String, &'static str) {
    let (year, era) = match date.year < 1 {
        true => (1 - date.year, " BC"),
        false => (date.year, ""),
    };
    let (month, day) = (date.month, date.day);
    (format!("{year:04}-{month:02}-{day:02}"), era)
}

/// The list `list` as the text of a Postgres array (`{"1","2",NULL}`),
/// a list of lists as one of more dimensions. Each element is quoted;
/// a null is `NULL`.
fn array_literal(caller: &str, list: &Data) -> Result<String, VmError> {
    let Data::Array(items) = list else {
        return scalar_text(caller, list);
    };
    let mut text = String::from("{");
    for (k, item) in items.iter().enumerate() {
        if k > 0 {
            text.push(',');
        }
        match item {
            Data::Null => text.push_str("NULL"),
            Data::Array(_) => text.push_str(&array_literal(caller, item)?),
            _ => {
                text.push('"');
                for c in scalar_text(caller, item)?.chars() {
                    if matches!(c, '"' | '\\') {
                        text.push('\\');
                    }
                    text.push(c);
                }
                text.push('"');
            }
        }
    }
    text.push('}');
    Ok(text)
}

/// `bytes` as the text of a `bytea`: `\x00ff`.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(2 + 2 * bytes.len());
    text.push_str("\\x");
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

// ── The type of a row ───────────────────────────────────────────────

/// The type a row is decoded into.
#[derive(Clone)]
pub(super) enum RowType {
    /// A record type of the program.
    Record(Arc<TypeInfo>),
    /// `json.Value`: a row is a `json.Object` of its columns.
    Value,
}

impl RowType {
    /// The row type that the type argument `ty` of `caller` names.
    pub(super) fn of(caller: &decode::Decoder, ty: Type) -> Result<RowType, VmError> {
        Ok(match decode::target(caller, ty)? {
            decode::Target::Record(ty, _) => RowType::Record(ty.clone()),
            // (`json.Value`: a postgres function takes no primitive type.)
            decode::Target::Value(_) => RowType::Value,
        })
    }
}

// ── Columns ─────────────────────────────────────────────────────────

/// What a column holds, as the decoder reads it. A domain's column holds
/// what its base type's does, an enum's holds text.
#[derive(Clone, Debug)]
enum Col {
    Bool,
    Int2,
    Int4,
    Int8,
    Float4,
    Float8,
    Numeric,
    Text,
    /// `"char"`, one byte.
    Char,
    Uuid,
    Bytea,
    Date,
    Time,
    Timestamp,
    Timestamptz,
    Json,
    Jsonb,
    Array(Box<Col>),
    /// `void`, what `pg_sleep(1)` gives: no value.
    Void,
    /// A type silt has no value for (`interval`, `inet`, `timetz`, ...).
    Other,
}

impl Col {
    fn of(ty: &PgType) -> Col {
        match *ty {
            PgType::BOOL => Col::Bool,
            PgType::INT2 => Col::Int2,
            PgType::INT4 => Col::Int4,
            PgType::INT8 => Col::Int8,
            PgType::FLOAT4 => Col::Float4,
            PgType::FLOAT8 => Col::Float8,
            PgType::NUMERIC => Col::Numeric,
            PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME => Col::Text,
            PgType::CHAR => Col::Char,
            PgType::UUID => Col::Uuid,
            PgType::BYTEA => Col::Bytea,
            PgType::DATE => Col::Date,
            PgType::TIME => Col::Time,
            PgType::TIMESTAMP => Col::Timestamp,
            PgType::TIMESTAMPTZ => Col::Timestamptz,
            PgType::JSON => Col::Json,
            PgType::JSONB => Col::Jsonb,
            PgType::VOID => Col::Void,
            _ => match ty.kind() {
                Kind::Array(element) => Col::Array(Box::new(Col::of(element))),
                Kind::Domain(base) => Col::of(base),
                Kind::Enum(_) => Col::Text,
                _ => Col::Other,
            },
        }
    }

    /// Whether a value of `field` can be decoded from what the column
    /// holds (a NULL aside).
    fn fills(&self, rt: &Runtime, field: &FieldType) -> bool {
        use FieldType as F;
        match (self, field) {
            (_, F::Option(inner)) => self.fills(rt, inner),
            (Col::Other, _) => false,
            (Col::Array(element), F::JsonValue) => element.fills(rt, field),
            (_, F::JsonValue) => true,
            (Col::Void, _) => false,
            // A json document fills a field of every type json's decoders
            // decode, in the record types the field names too.
            (Col::Json | Col::Jsonb, _) => {
                decode::decodes(&decode::Reader::new(rt), Family::Json, field)
            }
            (Col::Bool, F::Bool)
            | (Col::Int2 | Col::Int4 | Col::Int8, F::Int)
            | (Col::Float4 | Col::Float8 | Col::Numeric, F::Float)
            | (Col::Int2 | Col::Int4 | Col::Int8, F::Float)
            | (Col::Numeric | Col::Text | Col::Char | Col::Uuid, F::String)
            | (Col::Bytea, F::Bytes)
            | (Col::Date, F::Date)
            | (Col::Time, F::Time)
            | (Col::Timestamp | Col::Timestamptz, F::DateTime) => true,
            // An array of more dimensions fills a list of lists.
            (Col::Array(element), F::List(inner)) => {
                element.fills(rt, inner) || matches!(&**inner, F::List(_) if self.fills(rt, inner))
            }
            _ => false,
        }
    }
}

/// The name of the type `ty` as Postgres writes it, an array's as
/// `text[]`.
fn pg_name(ty: &PgType) -> String {
    match ty.kind() {
        Kind::Array(element) => format!("{}[]", pg_name(element)),
        _ => ty.name().to_string(),
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// `TypeMismatch(column, expected, actual)`.
fn type_mismatch(column: &str, expected: &str, actual: &str) -> Value {
    Value::variant(
        bv::PG_TYPE_MISMATCH,
        vec![
            Value::String(column.into()),
            Value::String(expected.into()),
            Value::String(actual.into()),
        ],
    )
}

/// `NoSuchColumn(column)`.
pub(super) fn no_such_column(column: &str) -> Value {
    Value::variant(bv::PG_NO_SUCH_COLUMN, vec![Value::String(column.into())])
}

/// The error of a result with two columns of the name `column`, which
/// a field reads.
fn twice(column: &str) -> Value {
    super::other_error(format!(
        "postgres: column `{column}` appears twice in the result; rename one with AS"
    ))
}

/// A cell that cannot be a value of the type `expected` (silt's text of
/// the part of the field's type that the cell was to be), because it is
/// `actual`.
struct Mismatch {
    expected: String,
    actual: String,
}

impl Mismatch {
    fn new(rt: &Runtime, expected: &FieldType, actual: impl Into<String>) -> Mismatch {
        Mismatch {
            expected: decode::shown(rt, expected),
            actual: actual.into(),
        }
    }
}

// ── The plan of a result ────────────────────────────────────────────

/// How the rows of one result are decoded: made from its columns before
/// any row is read.
pub(super) struct Plan {
    kind: PlanKind,
}

enum PlanKind {
    /// Each row a record of `ty`: each field, in the order the type
    /// declares them, from its column.
    Record {
        ty: Arc<TypeInfo>,
        /// The type of each field, and its column.
        fields: Vec<(FieldType, ColumnPlan)>,
    },
    /// Each row a `json.Object` of its columns.
    Value { columns: Vec<ColumnPlan> },
}

/// A column a row is read from.
struct ColumnPlan {
    /// Its name, which is that of the field it fills.
    name: String,
    /// Where it is in the row.
    column: usize,
    col: Col,
    /// Its type as Postgres names it.
    pg: String,
}

impl Plan {
    /// The plan of a result of `columns` whose rows are decoded into
    /// `row`, or the `PostgresError` that says why there is none: the
    /// first field, in the order of the type's declaration, that has no
    /// column (`NoSuchColumn`) or whose column's type does not fill it
    /// (`TypeMismatch`).
    pub(super) fn new(
        rt: &Runtime,
        row: &RowType,
        columns: &[tokio_postgres::Column],
    ) -> Result<Plan, Value> {
        let kind = match row {
            RowType::Value => {
                let mut seen = std::collections::HashSet::new();
                let mut plan = Vec::with_capacity(columns.len());
                for (at, column) in columns.iter().enumerate() {
                    // A `json.Object` reads every column.
                    if !seen.insert(column.name()) {
                        return Err(twice(column.name()));
                    }
                    // A column silt has no value for is refused at
                    // once, as it is for a record's field.
                    let (col, pg) = (Col::of(column.type_()), pg_name(column.type_()));
                    if !col.fills(rt, &FieldType::JsonValue) {
                        return Err(type_mismatch(column.name(), "json.Value", &pg));
                    }
                    plan.push(ColumnPlan {
                        name: column.name().to_string(),
                        column: at,
                        col,
                        pg,
                    });
                }
                PlanKind::Value { columns: plan }
            }
            RowType::Record(ty) => {
                let Shape::Record(declared) = &ty.shape else {
                    return Err(super::other_error(format!("{}: no record type", ty.name)));
                };
                // A field no decoder fills (in a record or enum type it
                // names too) is refused whatever the result holds, before
                // any column is matched. (The checker refuses such a
                // type wherever a program gives one: the decoders'
                // guard.)
                if let Some(message) =
                    decode::record_refusal(&decode::Reader::new(rt), Family::Postgres, ty)
                {
                    return Err(super::other_error(message));
                }
                let mut fields = Vec::with_capacity(declared.len());
                for (name, field) in declared {
                    let mut named = columns.iter().enumerate().filter(|(_, c)| c.name() == name);
                    let Some((column, found)) = named.next() else {
                        return Err(no_such_column(name));
                    };
                    if named.next().is_some() {
                        return Err(twice(name));
                    }
                    let (col, pg) = (Col::of(found.type_()), pg_name(found.type_()));
                    if !col.fills(rt, field) {
                        let expected = match field {
                            FieldType::Option(inner) => inner,
                            _ => field,
                        };
                        return Err(type_mismatch(name, &decode::shown(rt, expected), &pg));
                    }
                    fields.push((
                        field.clone(),
                        ColumnPlan {
                            name: name.clone(),
                            column,
                            col,
                            pg,
                        },
                    ));
                }
                PlanKind::Record {
                    ty: ty.clone(),
                    fields,
                }
            }
        };
        Ok(Plan { kind })
    }

    /// The value of `row`, or the `PostgresError` of a cell that has
    /// none (a NULL for a field that is no `Option`, a `NaN` for a
    /// `Float`).
    pub(super) fn row(&self, rt: &Runtime, row: &tokio_postgres::Row) -> Result<Value, Value> {
        let raw = |column: usize| match row.try_get::<_, Raw>(column) {
            Ok(Raw(raw)) => Ok(raw),
            Err(e) => Err(super::other_error(format!("postgres: {e}"))),
        };
        match &self.kind {
            PlanKind::Record { ty, fields } => {
                let mut values = Vec::with_capacity(fields.len());
                for (field, c) in fields {
                    let value = cell(rt, c, &c.col, field, raw(c.column)?)
                        .map_err(|m| type_mismatch(&c.name, &m.expected, &m.actual))?;
                    values.push(value);
                }
                Ok(Value::record(ty.clone(), values))
            }
            PlanKind::Value { columns } => {
                let mut entries = Vec::with_capacity(columns.len());
                for c in columns {
                    let value = cell(rt, c, &c.col, &FieldType::JsonValue, raw(c.column)?)
                        .map_err(|m| type_mismatch(&c.name, &m.expected, &m.actual))?;
                    entries.push((Value::String(c.name.as_str().into()), value));
                }
                Ok(Value::variant(bv::JSON_OBJECT, vec![Value::map(entries)]))
            }
        }
    }
}

/// A cell as the server sent it, in the binary format: `None` for NULL.
struct Raw<'a>(Option<&'a [u8]>);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(
        _ty: &PgType,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(Some(raw)))
    }

    fn from_sql_null(_ty: &PgType) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(None))
    }

    fn accepts(_ty: &PgType) -> bool {
        true
    }
}

// ── Cells ───────────────────────────────────────────────────────────

/// The value of the type `field` that the cell `raw` of the column `c`
/// is, which holds `col` (the column's own, or its elements'). The plan
/// has checked that `col` fills `field`.
fn cell(
    rt: &Runtime,
    c: &ColumnPlan,
    col: &Col,
    field: &FieldType,
    raw: Option<&[u8]>,
) -> Result<Value, Mismatch> {
    use FieldType as F;
    let Some(raw) = raw else {
        return match field {
            F::Option(_) => Ok(Value::variant(bv::NONE, Vec::new())),
            F::JsonValue => Ok(Value::variant(bv::JSON_NULL, Vec::new())),
            _ => Err(Mismatch::new(rt, field, "NULL")),
        };
    };
    let malformed = |_| Mismatch::new(rt, field, MALFORMED);
    Ok(match (col, field) {
        // A document is the field's as json.parse decodes it: `null` is
        // an `Option`'s `None`.
        (Col::Json | Col::Jsonb, _) => {
            let in_document = |why: String| Mismatch::new(rt, field, of_a_document(c, why));
            let document = document(col, raw).map_err(|_| in_document(MALFORMED.into()))?;
            json::decode_field(rt, &document, field).map_err(in_document)?
        }
        (_, F::Option(inner)) => {
            Value::variant(bv::SOME, vec![cell(rt, c, col, inner, Some(raw))?])
        }
        (_, F::JsonValue) => dynamic(rt, c, col, raw)?,
        (Col::Bool, F::Bool) => Value::Bool(wire::bool_from_sql(raw).map_err(malformed)?),
        (Col::Int2, F::Int) => Value::Int(wire::int2_from_sql(raw).map_err(malformed)?.into()),
        (Col::Int4, F::Int) => Value::Int(wire::int4_from_sql(raw).map_err(malformed)?.into()),
        (Col::Int8, F::Int) => Value::Int(wire::int8_from_sql(raw).map_err(malformed)?),
        (
            Col::Float4 | Col::Float8 | Col::Numeric | Col::Int2 | Col::Int4 | Col::Int8,
            F::Float,
        ) => {
            let f = float(col, raw).map_err(malformed)?;
            Value::Float(Float::new(f).ok_or_else(|| Mismatch::new(rt, field, non_finite(f)))?)
        }
        (Col::Numeric, F::String) => Value::String(numeric(raw).map_err(malformed)?.into()),
        (Col::Text | Col::Char | Col::Uuid, F::String) => {
            Value::String(text(col, raw).map_err(malformed)?.into())
        }
        (Col::Bytea, F::Bytes) => Value::Bytes(Arc::new(raw.to_vec())),
        (Col::Date, F::Date) => make_date(date(raw).map_err(|why| Mismatch::new(rt, field, why))?),
        (Col::Time, F::Time) => make_time(time(raw).map_err(|why| Mismatch::new(rt, field, why))?),
        (Col::Timestamp | Col::Timestamptz, F::DateTime) => {
            make_datetime(timestamp(raw).map_err(|why| Mismatch::new(rt, field, why))?)
        }
        (Col::Array(element), F::List(_)) => {
            let (dimensions, values) = array(raw).map_err(malformed)?;
            // An array of more dimensions, or of fewer, than the list
            // has levels (elements of a document aside, which may be
            // lists themselves) is the column's mismatch.
            let levels = std::iter::successors(Some(field), |t| match t {
                F::List(inner) => Some(&**inner),
                _ => None,
            });
            let levels = levels.filter(|t| matches!(t, F::List(_))).count();
            let documents = matches!(**element, Col::Json | Col::Jsonb);
            let fits = match documents {
                true => dimensions.len() <= levels,
                false => dimensions.is_empty() || dimensions.len() == levels,
            };
            let wrong = || {
                let n = dimensions.len();
                let plural = if n == 1 { "" } else { "s" };
                Mismatch::new(rt, field, format!("{} with {n} dimension{plural}", c.pg))
            };
            if !fits {
                return Err(wrong());
            }
            let mut values = values.into_iter();
            nest(&dimensions, field, &mut values, &mut |field, raw| {
                cell(rt, c, element, field, raw)
            })
            .map_err(|m| match m {
                Nested::Cell(m) => m,
                Nested::Dimensions => wrong(),
            })?
        }
        // (The plan has refused every other pair.)
        _ => return Err(Mismatch::new(rt, field, c.pg.clone())),
    })
}

/// The actual part of a type mismatch in a document of the column `c`
/// (a `json` or `jsonb` column, or an array of them): the column's type,
/// and `why` the document does not fit. The type at its head says that
/// the mismatch is a document's, which no cast in SQL mends
/// (`postgres::error_text`).
fn of_a_document(c: &ColumnPlan, why: String) -> String {
    format!("{}: {why}", c.pg)
}

/// What a cell that the server sent malformed is called in a type
/// mismatch.
const MALFORMED: &str = "a value the server sent malformed";

/// The `json.Value` of the cell `raw` of the column `c`, which holds
/// `col`: `json.Bool`, `json.Int`, `json.Float` as the column's type
/// says; a `numeric` as its exact text; text, a uuid, `bytea`'s hex text
/// and a date or a time as silt shows it as `json.String`; the document
/// of a `json` or `jsonb` column; an array as `json.Array`; `void` as
/// `json.Null`.
fn dynamic(rt: &Runtime, c: &ColumnPlan, col: &Col, raw: &[u8]) -> Result<Value, Mismatch> {
    let field = FieldType::JsonValue;
    let malformed = |_| Mismatch::new(rt, &field, MALFORMED);
    let string = |s: String| Value::variant(bv::JSON_STRING, vec![Value::String(s.into())]);
    Ok(match col {
        Col::Bool => Value::variant(
            bv::JSON_BOOL,
            vec![Value::Bool(wire::bool_from_sql(raw).map_err(malformed)?)],
        ),
        Col::Int2 | Col::Int4 | Col::Int8 => {
            let n = match col {
                Col::Int2 => wire::int2_from_sql(raw).map(i64::from),
                Col::Int4 => wire::int4_from_sql(raw).map(i64::from),
                _ => wire::int8_from_sql(raw),
            };
            Value::variant(bv::JSON_INT, vec![Value::Int(n.map_err(malformed)?)])
        }
        Col::Float4 | Col::Float8 => {
            let f = float(col, raw).map_err(malformed)?;
            let f = Float::new(f).ok_or_else(|| Mismatch::new(rt, &field, non_finite(f)))?;
            Value::variant(bv::JSON_FLOAT, vec![Value::Float(f)])
        }
        Col::Numeric => string(numeric(raw).map_err(malformed)?),
        Col::Text | Col::Char | Col::Uuid => string(text(col, raw).map_err(malformed)?),
        Col::Bytea => string(hex(raw)),
        Col::Date => {
            string(make_date(date(raw).map_err(|why| Mismatch::new(rt, &field, why))?).to_string())
        }
        Col::Time => {
            string(make_time(time(raw).map_err(|why| Mismatch::new(rt, &field, why))?).to_string())
        }
        Col::Timestamp | Col::Timestamptz => string(
            make_datetime(timestamp(raw).map_err(|why| Mismatch::new(rt, &field, why))?)
                .to_string(),
        ),
        Col::Json | Col::Jsonb => {
            let in_document = |_| Mismatch::new(rt, &field, of_a_document(c, MALFORMED.into()));
            decode::json_value(&document(col, raw).map_err(in_document)?)
        }
        Col::Void => Value::variant(bv::JSON_NULL, Vec::new()),
        Col::Array(element) => {
            let (dimensions, values) = array(raw).map_err(malformed)?;
            let mut values = values.into_iter();
            let mut leaf = |raw: Option<&[u8]>| match raw {
                Some(raw) => dynamic(rt, c, element, raw),
                None => Ok(Value::variant(bv::JSON_NULL, Vec::new())),
            };
            nest_dynamic(&dimensions, &mut values, &mut leaf)?
        }
        // (The plan has refused it.)
        Col::Other => return Err(Mismatch::new(rt, &field, c.pg.clone())),
    })
}

/// Why the elements of an array make no value.
enum Nested {
    Cell(Mismatch),
    /// The array has more dimensions than the list has levels.
    Dimensions,
}

/// The list of type `field` that the elements `values` of an array of
/// `dimensions` (the length of each) make, each element decoded by
/// `leaf` into the type of the innermost list's elements.
fn nest<'r>(
    dimensions: &[usize],
    field: &FieldType,
    values: &mut impl Iterator<Item = Option<&'r [u8]>>,
    leaf: &mut impl FnMut(&FieldType, Option<&'r [u8]>) -> Result<Value, Mismatch>,
) -> Result<Value, Nested> {
    let FieldType::List(element) = field else {
        return Err(Nested::Dimensions);
    };
    let Some((&length, inner)) = dimensions.split_first() else {
        // An empty array has no dimensions.
        return Ok(Value::list(Vec::new()));
    };
    let mut items = Vec::with_capacity(length);
    for _ in 0..length {
        items.push(match inner.is_empty() {
            true => leaf(element, values.next().flatten()).map_err(Nested::Cell)?,
            false => nest(inner, element, values, leaf)?,
        });
    }
    Ok(Value::list(items))
}

/// The `json.Array` that the elements `values` of an array of
/// `dimensions` make, each element decoded by `leaf`.
fn nest_dynamic<'r>(
    dimensions: &[usize],
    values: &mut impl Iterator<Item = Option<&'r [u8]>>,
    leaf: &mut impl FnMut(Option<&'r [u8]>) -> Result<Value, Mismatch>,
) -> Result<Value, Mismatch> {
    let array = |items| Value::variant(bv::JSON_ARRAY, vec![Value::list(items)]);
    let Some((&length, inner)) = dimensions.split_first() else {
        return Ok(array(Vec::new()));
    };
    let mut items = Vec::with_capacity(length);
    for _ in 0..length {
        items.push(match inner.is_empty() {
            true => leaf(values.next().flatten())?,
            false => nest_dynamic(inner, values, leaf)?,
        });
    }
    Ok(array(items))
}

type WireError = Box<dyn std::error::Error + Sync + Send>;

/// The dimensions (the length of each) and the elements of an array.
fn array(raw: &[u8]) -> Result<(Vec<usize>, Vec<Option<&[u8]>>), WireError> {
    let array = wire::array_from_sql(raw)?;
    let dimensions = array.dimensions().map(|d| Ok(usize::try_from(d.len)?));
    let dimensions: Vec<usize> = dimensions.collect()?;
    let values: Vec<Option<&[u8]>> = array.values().collect()?;
    let count: usize = match dimensions.is_empty() {
        true => 0,
        false => dimensions.iter().product(),
    };
    match count == values.len() {
        true => Ok((dimensions, values)),
        false => Err("the array's elements do not fill its dimensions".into()),
    }
}

/// The JSON document of a `json` or `jsonb` cell.
fn document(col: &Col, raw: &[u8]) -> Result<serde_json::Value, WireError> {
    let text = match (col, raw) {
        // A `jsonb` cell begins with the version of its format, 1.
        (Col::Jsonb, [1, text @ ..]) => text,
        (Col::Jsonb, _) => return Err("a jsonb cell of an unknown version".into()),
        _ => raw,
    };
    Ok(json::parse_document(text)?)
}

/// The text of a text, `"char"` or uuid cell.
fn text(col: &Col, raw: &[u8]) -> Result<String, WireError> {
    Ok(match col {
        Col::Char => {
            let byte = wire::char_from_sql(raw)? as u8;
            match byte.is_ascii() {
                true => char::from(byte).to_string(),
                // (Postgres's own text of such a byte.)
                false => format!("\\{byte:03o}"),
            }
        }
        Col::Uuid => uuid::Uuid::from_bytes(wire::uuid_from_sql(raw)?).to_string(),
        _ => wire::text_from_sql(raw)?.to_string(),
    })
}

/// The number of a `float4`, `float8`, `numeric` or integer cell: a
/// `numeric`'s or an `int8`'s nearest `f64`, as `::float8` gives it.
fn float(col: &Col, raw: &[u8]) -> Result<f64, WireError> {
    Ok(match col {
        Col::Float4 => wire::float4_from_sql(raw)?.into(),
        Col::Float8 => wire::float8_from_sql(raw)?,
        Col::Int2 => wire::int2_from_sql(raw)?.into(),
        Col::Int4 => wire::int4_from_sql(raw)?.into(),
        Col::Int8 => wire::int8_from_sql(raw)? as f64,
        _ => match numeric(raw)?.as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            exact => exact.parse()?,
        },
    })
}

/// What a number that is no `Float` is called in a type mismatch.
fn non_finite(f: f64) -> &'static str {
    match f {
        f if f.is_nan() => "NaN",
        f if f > 0.0 => "Infinity",
        _ => "-Infinity",
    }
}

/// The exact text of a `numeric` cell, as Postgres writes it (`12.50`,
/// `NaN`, `-Infinity`). The binary format is the number's digits in
/// base 10000, the first of them at the power `weight`, and the count
/// of decimal digits after the point (`scale`).
fn numeric(raw: &[u8]) -> Result<String, WireError> {
    let word = |at: usize| -> Result<u16, WireError> {
        match raw.get(at..at + 2) {
            Some(&[hi, lo]) => Ok(u16::from_be_bytes([hi, lo])),
            _ => Err("a numeric cell too short".into()),
        }
    };
    let count = usize::from(word(0)?);
    let weight = i64::from(word(2)? as i16);
    let sign = word(4)?;
    let scale = usize::from(word(6)?);
    let digits: Vec<u16> = (0..count)
        .map(|k| word(8 + 2 * k))
        .collect::<Result<_, _>>()?;
    let digit = |power: i64| -> u16 {
        // The digit at `power` of 10000.
        usize::try_from(weight - power)
            .ok()
            .and_then(|k| digits.get(k).copied())
            .unwrap_or(0)
    };
    let mut text = String::new();
    match sign {
        0x0000 => {}
        0x4000 => text.push('-'),
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => return Err("a numeric cell of an unknown sign".into()),
    }
    match weight < 0 {
        true => text.push('0'),
        false => {
            let _ = write!(text, "{}", digit(weight));
            for power in (0..weight).rev() {
                let _ = write!(text, "{:04}", digit(power));
            }
        }
    }
    if scale > 0 {
        let mut fraction = String::with_capacity(scale + 4);
        let mut power = -1;
        while fraction.len() < scale {
            let _ = write!(fraction, "{:04}", digit(power));
            power -= 1;
        }
        fraction.truncate(scale);
        text.push('.');
        text.push_str(&fraction);
    }
    Ok(text)
}

/// Postgres's dates count days from 2000-01-01, its times microseconds.
const EPOCH: NaiveDate = match NaiveDate::from_ymd_opt(2000, 1, 1) {
    Some(date) => date,
    None => unreachable!(),
};

/// The date of a `date` cell, or what it is instead.
fn date(raw: &[u8]) -> Result<NaiveDate, String> {
    let days = wire::date_from_sql(raw).map_err(|e| e.to_string())?;
    match days {
        // (Named as a `Float`'s are.)
        i32::MAX => Err("Infinity".into()),
        i32::MIN => Err("-Infinity".into()),
        days => EPOCH
            .checked_add_signed(TimeDelta::days(days.into()))
            .ok_or_else(|| "a date out of Date's range".into()),
    }
}

/// The time of a `time` cell, or what it is instead.
fn time(raw: &[u8]) -> Result<NaiveTime, String> {
    let micros = wire::time_from_sql(raw).map_err(|e| e.to_string())?;
    let (seconds, micros) = (micros.div_euclid(1_000_000), micros.rem_euclid(1_000_000));
    u32::try_from(seconds)
        .ok()
        .and_then(|s| NaiveTime::from_num_seconds_from_midnight_opt(s, (micros * 1000) as u32))
        .ok_or_else(|| "24:00:00".into())
}

/// The date and time of a `timestamp` or `timestamptz` cell (in UTC),
/// or what it is instead.
fn timestamp(raw: &[u8]) -> Result<NaiveDateTime, String> {
    let micros = wire::timestamp_from_sql(raw).map_err(|e| e.to_string())?;
    match micros {
        i64::MAX => Err("Infinity".into()),
        i64::MIN => Err("-Infinity".into()),
        micros => EPOCH
            .and_time(NaiveTime::MIN)
            .checked_add_signed(TimeDelta::microseconds(micros))
            .ok_or_else(|| "a datetime out of DateTime's range".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary format of a `numeric`: its digits in base 10000.
    fn numeric_cell(weight: i16, sign: u16, scale: u16, digits: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(digits.len() as u16).to_be_bytes());
        raw.extend_from_slice(&weight.to_be_bytes());
        raw.extend_from_slice(&sign.to_be_bytes());
        raw.extend_from_slice(&scale.to_be_bytes());
        for digit in digits {
            raw.extend_from_slice(&digit.to_be_bytes());
        }
        raw
    }

    #[test]
    fn a_numeric_is_its_exact_text() {
        let text = |weight, sign, scale, digits: &[u16]| {
            numeric(&numeric_cell(weight, sign, scale, digits)).expect("a numeric")
        };
        assert_eq!(text(0, 0, 2, &[12, 5000]), "12.50");
        assert_eq!(text(-1, 0, 4, &[1]), "0.0001");
        assert_eq!(text(-2, 0, 6, &[1]), "0.000000");
        assert_eq!(text(-2, 0, 8, &[1]), "0.00000001");
        assert_eq!(text(1, 0, 0, &[1]), "10000");
        assert_eq!(
            text(2, 0x4000, 6, &[1, 2345, 6789, 1, 2300]),
            "-123456789.000123"
        );
        assert_eq!(text(0, 0, 0, &[]), "0");
        assert_eq!(text(0, 0, 2, &[]), "0.00");
        assert_eq!(text(0, 0xC000, 0, &[]), "NaN");
        assert_eq!(text(0, 0xD000, 0, &[]), "Infinity");
        assert_eq!(text(0, 0xF000, 0, &[]), "-Infinity");
        assert!(numeric(&[0, 1]).is_err());
    }

    /// A json or jsonb cell is read as `json.parse` reads a text: a
    /// number is an `Int` only when its text is a whole number in range.
    #[test]
    fn a_number_of_a_json_cell_is_read_exactly() {
        for col in [Col::Json, Col::Jsonb] {
            let cell = |text: &str| {
                // (A jsonb cell begins with the version of its format.)
                let mut raw = match matches!(col, Col::Jsonb) {
                    true => vec![1],
                    false => Vec::new(),
                };
                raw.extend_from_slice(text.as_bytes());
                decode::json_value(&document(&col, &raw).expect("a document")).to_string()
            };
            assert_eq!(cell("9007199254740993.0"), "Int(9007199254740993)");
            assert_eq!(
                cell("-9223372036854775809"),
                "Float(-9223372036854776000.0)"
            );
            assert_eq!(cell("1.0000000000000001"), "Float(1.0)");
        }
    }

    /// `value` as the data of a parameter.
    fn pg(value: &Value) -> Data {
        data::data("postgres.query", data::Format::Postgres, value).expect("data")
    }

    #[test]
    fn a_list_is_an_array_literal_with_every_element_quoted() {
        let list = Value::list(vec![
            Value::String("a\"b".into()),
            Value::String("c\\d".into()),
            Value::variant(bv::NONE, Vec::new()),
            Value::variant(bv::SOME, vec![Value::Int(1)]),
        ]);
        assert_eq!(
            array_literal("postgres.query", &pg(&list)).expect("written"),
            r#"{"a\"b","c\\d",NULL,"1"}"#
        );
        let grid = Value::list(vec![
            Value::list(vec![Value::Int(1), Value::Int(2)]),
            Value::list(vec![Value::Int(3), Value::Int(4)]),
        ]);
        assert_eq!(
            array_literal("postgres.query", &pg(&grid)).expect("written"),
            r#"{{"1","2"},{"3","4"}}"#
        );
        assert_eq!(
            array_literal("postgres.query", &pg(&Value::list(Vec::new()))).expect("written"),
            "{}"
        );
    }

    /// A range too long to write out is refused where the parameter is
    /// to be an array too, as json refuses it.
    #[test]
    fn a_range_too_long_to_write_out_is_no_parameter() {
        let range = Value::List(crate::value::List::ints(1, 10_000_001).expect("a range"));
        let refused = written(&range).unwrap_err();
        assert_eq!(
            refused.message,
            "range 1..10000001 has 10000001 elements; materializing more than 10000000 is not allowed"
        );
    }

    /// A datetime goes out in UTC, with its offset; a year before 1 as
    /// a year BC (year 0 is 1 BC).
    #[test]
    fn dates_are_written_as_postgres_reads_them() {
        let text = |value: Value| scalar_text("postgres.query", &pg(&value)).expect("written");
        let day = |y, m, d| make_date(NaiveDate::from_ymd_opt(y, m, d).expect("a date"));
        let at = |y, m, d| {
            let date = NaiveDate::from_ymd_opt(y, m, d).expect("a date");
            make_datetime(date.and_hms_milli_opt(12, 0, 0, 250).expect("a time"))
        };
        assert_eq!(text(day(2026, 10, 19)), "2026-10-19");
        assert_eq!(text(day(10000, 1, 1)), "10000-01-01");
        assert_eq!(text(day(-43, 3, 15)), "0044-03-15 BC");
        assert_eq!(text(day(0, 12, 31)), "0001-12-31 BC");
        assert_eq!(text(at(2026, 10, 19)), "2026-10-19T12:00:00.250000000+00");
        assert_eq!(text(at(-43, 3, 15)), "0044-03-15T12:00:00.250000000+00 BC");
        let time = make_time(NaiveTime::from_hms_opt(1, 2, 3).expect("a time"));
        assert_eq!(text(time), "01:02:03");
    }

    /// What `f`, the builtin `json.stringify` or `toml.stringify`,
    /// writes of `#{"v": value}`: the text, or the message of its
    /// refusal (json raises, toml returns `Err(Other(message))`).
    fn written_by(
        f: fn(&mut crate::vm::Vm, &[Value]) -> crate::builtins::typed::Called,
        value: &Value,
    ) -> Result<String, String> {
        let mut vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        let doc = Value::map([(Value::String("v".into()), value.clone())]);
        let written = match f(&mut vm, &[doc]) {
            Some(Ok(crate::vm::Step::Done(written))) => written,
            Some(Err(e)) => return Err(e.message),
            _ => panic!("no result"),
        };
        match written {
            Value::String(text) => Ok(text.to_string()),
            Value::Variant(result) if result.is(bv::OK) => match result.fields() {
                [Value::String(text)] => Ok(text.to_string()),
                other => panic!("{other:?}"),
            },
            Value::Variant(result) => match result.fields() {
                [Value::Variant(other)] => match other.fields() {
                    [Value::String(message)] => Err(message.to_string()),
                    fields => panic!("{fields:?}"),
                },
                fields => panic!("{fields:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    /// The parameter `value`, written as `postgres.query` writes it.
    fn written(value: &Value) -> Result<TextParam, VmError> {
        let caller = "postgres.query";
        let value = data::data(caller, data::Format::Postgres, value)?;
        param(caller, value, &Unsendable::default())
    }

    /// The text of the parameter `value`, or the message of its refusal.
    fn sent(value: &Value) -> Result<String, String> {
        match written(value) {
            Ok(TextParam::Text(text)) => Ok(text),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e.message),
        }
    }

    /// What a refusal says, without the function it names.
    fn said(message: &str) -> &str {
        message.split_once(": ").expect("a caller").1
    }

    /// One value of every kind written by `json.stringify`,
    /// `toml.stringify` and as a parameter: the three agree on what they
    /// share, the scalars, the dates and times (each format in its own
    /// form: toml's literal, postgres's `+00`), and the refusals.
    #[test]
    fn json_toml_and_a_parameter_agree_on_a_value() {
        use crate::builtins::{json as j, toml as t};
        use chrono::NaiveDateTime;
        // How a date or time is compared: as what it reads back as.
        #[derive(Clone, Copy)]
        enum Read {
            Text,
            Time,
            DateTime,
        }
        let date = |y, m, d| make_date(NaiveDate::from_ymd_opt(y, m, d).expect("a date"));
        let time = NaiveTime::from_hms_milli_opt(12, 30, 0, 250).expect("a time");
        let at = NaiveDate::from_ymd_opt(2026, 10, 19)
            .expect("a date")
            .and_hms_milli_opt(12, 30, 5, 500)
            .expect("a time");
        let float = |f| Value::Float(Float::new(f).expect("finite"));
        let shared = [
            (Value::Int(42), Read::Text, "42"),
            (Value::Int(-7), Read::Text, "-7"),
            (float(2.5), Read::Text, "2.5"),
            (float(2.0), Read::Text, "2.0"),
            (Value::Bool(true), Read::Text, "true"),
            (Value::String("a \"b\" ü".into()), Read::Text, "a \"b\" ü"),
            (
                Value::variant(bv::SOME, vec![Value::Int(3)]),
                Read::Text,
                "3",
            ),
            (
                Value::variant(bv::JSON_INT, vec![Value::Int(5)]),
                Read::Text,
                "5",
            ),
            (date(2026, 10, 19), Read::Text, "2026-10-19"),
            // (No TOML literal holds the year: a string of the text.)
            (date(12345, 3, 15), Read::Text, "12345-03-15"),
            (make_time(time), Read::Time, "12:30:00.250"),
            (make_datetime(at), Read::DateTime, "2026-10-19 12:30:05.500"),
        ];
        for (value, read, expected) in shared {
            let json = written_by(j::stringify, &value).expect("json");
            let toml = written_by(t::stringify, &value).expect("toml");
            let json = match &serde_json::from_str::<serde_json::Value>(&json).expect("JSON")["v"] {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let toml = match ::toml::from_str::<::toml::Value>(&toml)
                .expect("TOML")
                .get("v")
            {
                Some(::toml::Value::String(s)) => s.clone(),
                Some(::toml::Value::Float(f)) => format!("{f:?}"),
                Some(::toml::Value::Datetime(at)) => at.to_string(),
                Some(other) => other.to_string(),
                None => panic!("no v in {toml}"),
            };
            let pg = sent(&value).expect("a parameter");
            let texts = match read {
                Read::Text => [json, toml, pg],
                Read::Time => [json, toml, pg].map(|text| {
                    decode::parse_time(&text)
                        .expect("a time")
                        .format("%H:%M:%S%.3f")
                        .to_string()
                }),
                // (Postgres's own: the offset of UTC.)
                Read::DateTime => {
                    [json, toml, pg.strip_suffix("+00").expect("+00").into()].map(|text| {
                        let at: NaiveDateTime = decode::parse_datetime(&text).expect("a datetime");
                        at.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
                    })
                }
            };
            assert_eq!(texts, [expected; 3].map(String::from), "{value:?}");
        }

        // The value each refuses: as `#{"v": doc}` for json and toml, as
        // `param` for postgres (`Bytes` is a parameter, not inside one;
        // the nested list is as deep as the others'). On the smallest
        // stack a thread that runs silt code has, as the deepest value
        // takes megabytes of it in a debug build.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let bytes = Value::Bytes(Arc::new(vec![1, 2]));
                let v = |x: Value| Value::map([(Value::String("v".into()), x)]);
                let nested = (0..crate::builtins::data::MAX_ENCODE_DEPTH)
                    .fold(Value::Int(1), |inner, _| Value::list(vec![inner]));
                let refused = [
                    (
                        Value::set([Value::Int(1)]),
                        None,
                        "unsupported value kind Set",
                    ),
                    (
                        bytes.clone(),
                        Some(v(bytes)),
                        "unsupported value kind Bytes",
                    ),
                    (
                        nested.clone(),
                        Some(v(nested)),
                        "the value is nested more than 1000 levels deep",
                    ),
                    (
                        Value::List(crate::value::List::ints(1, 10_000_001).expect("a range")),
                        None,
                        "range 1..10000001 has 10000001 elements; \
                         materializing more than 10000000 is not allowed",
                    ),
                ];
                for (doc, param, expected) in refused {
                    let json = written_by(j::stringify, &doc).expect_err(expected);
                    let toml = written_by(t::stringify, &doc).expect_err(expected);
                    let pg = sent(param.as_ref().unwrap_or(&doc)).expect_err(expected);
                    let texts = [json, toml, pg];
                    let texts = texts
                        .each_ref()
                        .map(|text| match text.starts_with("range ") {
                            true => text.as_str(),
                            false => said(text),
                        });
                    assert_eq!(texts, [expected; 3]);
                }
            })
            .expect("a thread")
            .join()
            .expect("the refusals agree");
    }
}
