// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Decodes IPC traffic using an interface's `.idol` definition.
//!
//! Hubris IPC on the wire is an operation number plus a byte string, which
//! is opaque to anything watching syscalls. Given the `.idol` file that
//! defines an interface, this crate turns a request into
//! `post(id: SensorId(3), value: 31.5, timestamp: 1000)` and a reply into
//! `Ok(31.5)` or `Err(SensorError::NoReading)`, for host fixtures and traces
//! to print, and for fixtures that stand in for a server to act on.
//!
//! An `.idol` file only *names* the argument types. Primitives, tuples,
//! arrays, `Option` and `Result` decode on their own; anything else needs a
//! [`TypeDef`] in the [`TypeRegistry`], which starts out knowing the sensor
//! API's types. Encoding follows the operation's declared `encoding`: hubpack
//! and ssmarshal (fixed-width little-endian integers, one-byte variant
//! indices and option tags) or zerocopy (a packed struct, in host layout,
//! since the tasks producing it are host builds).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use idol::syntax::{Encoding, Error as ErrorSpec, Interface, Operation, Reply};

/// Layout of a type that an interface mentions only by name.
#[derive(Debug, Clone)]
pub enum TypeDef {
    /// A newtype over `inner`, printed as `Name(inner)`.
    Newtype(String),
    /// A field-less enum, encoded as its variant index; names in order.
    Enum(Vec<String>),
    /// A struct with named fields, in declaration order.
    Struct(Vec<(String, String)>),
    /// An error enum used as a reply code: `(code, variant)` pairs.
    ErrorCodes(Vec<(u32, String)>),
}

/// Named types the decoder knows about.
#[derive(Debug, Clone, Default)]
pub struct TypeRegistry {
    types: BTreeMap<String, TypeDef>,
}

impl TypeRegistry {
    /// A registry knowing the types of the `Sensor` interface.
    pub fn builtin() -> Self {
        let mut r = Self::default();
        r.insert("SensorId", TypeDef::Newtype("u32".into()));
        r.insert(
            "NoData",
            TypeDef::Enum(
                [
                    "DeviceOff",
                    "DeviceError",
                    "DeviceNotPresent",
                    "DeviceUnavailable",
                    "DeviceTimeout",
                ]
                .map(String::from)
                .to_vec(),
            ),
        );
        r.insert(
            "Reading",
            TypeDef::Struct(vec![
                ("timestamp".into(), "u64".into()),
                ("value".into(), "f32".into()),
            ]),
        );
        r.insert(
            "SensorError",
            TypeDef::ErrorCodes(vec![
                (2, "NoReading".into()),
                (3, "NotPresent".into()),
                (4, "DeviceError".into()),
                (5, "DeviceUnavailable".into()),
                (6, "DeviceTimeout".into()),
                (7, "DeviceOff".into()),
            ]),
        );
        r
    }

    pub fn insert(&mut self, name: &str, def: TypeDef) {
        self.types.insert(name.to_string(), def);
    }

    pub fn get(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(name)
    }
}

/// A decoded value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i128),
    Uint(u128),
    Float(f64),
    /// `Name(inner)`.
    Newtype(String, Box<Value>),
    /// `Type::Variant`, by index.
    Variant {
        ty: String,
        index: u8,
        name: String,
    },
    Struct(String, Vec<(String, Value)>),
    Tuple(Vec<Value>),
    Array(Vec<Value>),
    Option(Option<Box<Value>>),
    Result(Result<Box<Value>, Box<Value>>),
    /// Bytes that could not be decoded.
    Raw(Vec<u8>),
}

impl Value {
    /// The integer inside a `Uint`, `Int`, or a newtype over one.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Uint(v) => u64::try_from(*v).ok(),
            Value::Int(v) => u64::try_from(*v).ok(),
            Value::Newtype(_, inner) => inner.as_u64(),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Float(v) => Some(*v as f32),
            Value::Newtype(_, inner) => inner.as_f32(),
            _ => None,
        }
    }

    /// The variant index of an enum value.
    pub fn variant_index(&self) -> Option<u8> {
        match self {
            Value::Variant { index, .. } => Some(*index),
            Value::Newtype(_, inner) => inner.variant_index(),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Unit => write!(f, "()"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Uint(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Newtype(name, inner) => write!(f, "{name}({inner})"),
            Value::Variant { ty, name, .. } => write!(f, "{ty}::{name}"),
            Value::Struct(name, fields) => {
                write!(f, "{name} {{ ")?;
                for (i, (field, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {value}")?;
                }
                write!(f, " }}")
            }
            Value::Tuple(items) | Value::Array(items) => {
                let (open, close) = if matches!(self, Value::Tuple(_)) {
                    ("(", ")")
                } else {
                    ("[", "]")
                };
                write!(f, "{open}")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "{close}")
            }
            Value::Option(None) => write!(f, "None"),
            Value::Option(Some(v)) => write!(f, "Some({v})"),
            Value::Result(Ok(v)) => write!(f, "Ok({v})"),
            Value::Result(Err(e)) => write!(f, "Err({e})"),
            Value::Raw(bytes) => {
                write!(f, "<{} bytes:", bytes.len())?;
                for b in bytes {
                    write!(f, " {b:02x}")?;
                }
                write!(f, ">")
            }
        }
    }
}

/// A type as an interface names it, reduced to what decoding needs.
#[derive(Debug, Clone)]
enum TypeExpr {
    Unit,
    Tuple(Vec<TypeExpr>),
    Array(Box<TypeExpr>, usize),
    /// A path type by its last segment, with generic type arguments.
    Named(String, Vec<TypeExpr>),
}

impl TypeExpr {
    fn from_syn(ty: &syn::Type) -> Result<Self> {
        Ok(match ty {
            syn::Type::Tuple(t) if t.elems.is_empty() => TypeExpr::Unit,
            syn::Type::Tuple(t) => TypeExpr::Tuple(
                t.elems.iter().map(Self::from_syn).collect::<Result<_>>()?,
            ),
            syn::Type::Array(a) => {
                let len = match &a.len {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Int(n),
                        ..
                    }) => n.base10_parse::<usize>()?,
                    _ => bail!("array length is not a literal"),
                };
                TypeExpr::Array(Box::new(Self::from_syn(&a.elem)?), len)
            }
            syn::Type::Paren(p) => Self::from_syn(&p.elem)?,
            syn::Type::Path(p) => {
                let segment = p
                    .path
                    .segments
                    .last()
                    .ok_or_else(|| anyhow!("empty type path"))?;
                let generics = match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => args
                        .args
                        .iter()
                        .filter_map(|a| match a {
                            syn::GenericArgument::Type(t) => {
                                Some(Self::from_syn(t))
                            }
                            _ => None,
                        })
                        .collect::<Result<_>>()?,
                    _ => Vec::new(),
                };
                TypeExpr::Named(segment.ident.to_string(), generics)
            }
            other => bail!("unsupported type {other:?}"),
        })
    }

    fn parse(text: &str) -> Result<Self> {
        let ty: syn::Type = syn::parse_str(text)
            .map_err(|e| anyhow!("bad type {text:?}: {e}"))?;
        Self::from_syn(&ty)
    }
}

#[derive(Debug, Clone)]
enum ReplyDef {
    Simple(TypeExpr),
    Result { ok: TypeExpr, err: ErrorDef },
}

#[derive(Debug, Clone)]
enum ErrorDef {
    /// Error type name; the reply code identifies the variant.
    CLike(String),
    Complex(TypeExpr),
    ServerDeath,
}

#[derive(Debug, Clone)]
struct OpDef {
    name: String,
    args: Vec<(String, TypeExpr)>,
    reply: ReplyDef,
    encoding: Encoding,
}

impl OpDef {
    fn from_idol(name: &str, op: &Operation) -> Result<Self> {
        let args = op
            .args
            .iter()
            .map(|(n, a)| Ok((n.to_string(), TypeExpr::from_syn(&a.ty.0)?)))
            .collect::<Result<_>>()?;
        let reply = match &op.reply {
            Reply::Simple(ty) => {
                ReplyDef::Simple(TypeExpr::from_syn(&ty.ty.0)?)
            }
            Reply::Result { ok, err } => ReplyDef::Result {
                ok: TypeExpr::from_syn(&ok.ty.0)?,
                err: match err {
                    ErrorSpec::CLike(ty) => ErrorDef::CLike(
                        last_segment(&ty.0).unwrap_or_else(|| ty.to_string()),
                    ),
                    ErrorSpec::Complex(ty) => {
                        ErrorDef::Complex(TypeExpr::from_syn(&ty.0)?)
                    }
                    ErrorSpec::ServerDeath => ErrorDef::ServerDeath,
                },
            },
        };
        Ok(Self {
            name: name.to_string(),
            args,
            reply,
            encoding: op.encoding,
        })
    }
}

/// An interface definition ready to decode traffic.
///
/// Holds only plain data, so it can live in a static.
pub struct Decoder {
    name: String,
    /// Operations in discriminator order (operation 1 is index 0).
    ops: Vec<OpDef>,
    registry: TypeRegistry,
}

impl Decoder {
    /// Loads an interface from its `.idol` file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let interface: Interface = text
            .parse()
            .map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
        Self::new(&interface, TypeRegistry::builtin())
    }

    pub fn new(interface: &Interface, registry: TypeRegistry) -> Result<Self> {
        let ops = interface
            .ops
            .iter()
            .map(|(name, op)| OpDef::from_idol(&name.to_string(), op))
            .collect::<Result<_>>()?;
        Ok(Self {
            name: interface.name.to_string(),
            ops,
            registry,
        })
    }

    pub fn registry_mut(&mut self) -> &mut TypeRegistry {
        &mut self.registry
    }

    /// The interface's name, e.g. `Sensor`.
    pub fn interface_name(&self) -> &str {
        &self.name
    }

    pub fn op_name(&self, op: u16) -> Option<&str> {
        self.ops
            .get(usize::from(op).checked_sub(1)?)
            .map(|op| op.name.as_str())
    }

    fn op(&self, op: u16) -> Result<&OpDef> {
        usize::from(op)
            .checked_sub(1)
            .and_then(|i| self.ops.get(i))
            .ok_or_else(|| anyhow!("{} has no operation {op}", self.name))
    }

    /// Decodes a request's arguments, in declaration order.
    pub fn decode_args(
        &self,
        op: u16,
        body: &[u8],
    ) -> Result<Vec<(String, Value)>> {
        let op = self.op(op)?;
        let mut input = body;
        let mut out = Vec::new();
        for (name, ty) in &op.args {
            let value = self
                .decode(ty, op.encoding, &mut input)
                .with_context(|| format!("argument {name}"))?;
            out.push((name.clone(), value));
        }
        if !input.is_empty() {
            bail!("{} bytes left over after the arguments", input.len());
        }
        Ok(out)
    }

    /// Renders a request as `name(arg: value, ...)`.
    pub fn describe_request(&self, op: u16, body: &[u8]) -> String {
        let Ok(def) = self.op(op) else {
            return format!("op {op} {}", Value::Raw(body.to_vec()));
        };
        match self.decode_args(op, body) {
            Ok(args) => {
                let args: Vec<String> =
                    args.iter().map(|(n, v)| format!("{n}: {v}")).collect();
                format!("{}({})", def.name, args.join(", "))
            }
            Err(_) => format!("{}({})", def.name, Value::Raw(body.to_vec())),
        }
    }

    /// Decodes a reply to `op` with response code `code`.
    pub fn decode_reply(
        &self,
        op: u16,
        code: u32,
        body: &[u8],
    ) -> Result<Value> {
        let op = self.op(op)?;
        if let Some(generation) = abi_dead_generation(code) {
            bail!("peer restarted (generation {generation})");
        }
        let mut input = body;
        Ok(match &op.reply {
            ReplyDef::Simple(ty) => {
                if code != 0 {
                    bail!("response code {code} on an infallible operation");
                }
                self.decode(ty, op.encoding, &mut input)?
            }
            ReplyDef::Result { ok, .. } if code == 0 => Value::Result(Ok(
                Box::new(self.decode(ok, op.encoding, &mut input)?),
            )),
            ReplyDef::Result { err, .. } => {
                Value::Result(Err(Box::new(match err {
                    ErrorDef::CLike(ty_name) => {
                        let variant = match self.registry.get(ty_name) {
                            Some(TypeDef::ErrorCodes(codes)) => codes
                                .iter()
                                .find(|(c, _)| *c == code)
                                .map(|(_, v)| format!("{ty_name}::{v}")),
                            _ => None,
                        };
                        match variant {
                            Some(v) => Value::Newtype(v, Box::new(Value::Unit)),
                            None => Value::Newtype(
                                ty_name.clone(),
                                Box::new(Value::Uint(code.into())),
                            ),
                        }
                    }
                    ErrorDef::Complex(ty) => {
                        self.decode(ty, op.encoding, &mut input)?
                    }
                    ErrorDef::ServerDeath => Value::Newtype(
                        "ServerDeath".into(),
                        Box::new(Value::Uint(code.into())),
                    ),
                })))
            }
        })
    }

    /// Renders a reply: `Ok(value)`, `Err(Type::Variant)`, or the plain value.
    pub fn describe_reply(&self, op: u16, code: u32, body: &[u8]) -> String {
        match self.decode_reply(op, code, body) {
            Ok(Value::Result(Err(e))) => match *e {
                Value::Newtype(name, inner) if *inner == Value::Unit => {
                    format!("Err({name})")
                }
                other => format!("Err({other})"),
            },
            Ok(v) => v.to_string(),
            Err(e) => {
                format!("code {code} {} ({e})", Value::Raw(body.to_vec()))
            }
        }
    }

    fn decode(
        &self,
        ty: &TypeExpr,
        encoding: Encoding,
        input: &mut &[u8],
    ) -> Result<Value> {
        match ty {
            TypeExpr::Unit => Ok(Value::Unit),
            TypeExpr::Tuple(elems) => Ok(Value::Tuple(
                elems
                    .iter()
                    .map(|e| self.decode(e, encoding, input))
                    .collect::<Result<_>>()?,
            )),
            TypeExpr::Array(elem, len) => Ok(Value::Array(
                (0..*len)
                    .map(|_| self.decode(elem, encoding, input))
                    .collect::<Result<_>>()?,
            )),
            TypeExpr::Named(name, generics) => {
                self.decode_named(name, generics, encoding, input)
            }
        }
    }

    fn decode_named(
        &self,
        name: &str,
        generics: &[TypeExpr],
        encoding: Encoding,
        input: &mut &[u8],
    ) -> Result<Value> {
        let serde_like = !matches!(encoding, Encoding::Zerocopy);
        Ok(match name {
            "bool" => Value::Bool(take(input, 1)?[0] != 0),
            "u8" => Value::Uint(take(input, 1)?[0].into()),
            "i8" => Value::Int((take(input, 1)?[0] as i8).into()),
            "u16" => Value::Uint(
                u16::from_le_bytes(take(input, 2)?.try_into()?).into(),
            ),
            "i16" => Value::Int(
                i16::from_le_bytes(take(input, 2)?.try_into()?).into(),
            ),
            "u32" => Value::Uint(
                u32::from_le_bytes(take(input, 4)?.try_into()?).into(),
            ),
            "i32" => Value::Int(
                i32::from_le_bytes(take(input, 4)?.try_into()?).into(),
            ),
            // Host tasks and serde both use 64 bits for usize.
            "u64" | "usize" => Value::Uint(
                u64::from_le_bytes(take(input, 8)?.try_into()?).into(),
            ),
            "i64" | "isize" => Value::Int(
                i64::from_le_bytes(take(input, 8)?.try_into()?).into(),
            ),
            "f32" => Value::Float(
                f32::from_le_bytes(take(input, 4)?.try_into()?).into(),
            ),
            "f64" => {
                Value::Float(f64::from_le_bytes(take(input, 8)?.try_into()?))
            }
            "Option" => {
                if !serde_like {
                    bail!("Option cannot be zerocopy-encoded");
                }
                let [inner] = generics else {
                    bail!("Option needs one type argument")
                };
                match take(input, 1)?[0] {
                    0 => Value::Option(None),
                    1 => Value::Option(Some(Box::new(
                        self.decode(inner, encoding, input)?,
                    ))),
                    tag => bail!("bad Option tag {tag}"),
                }
            }
            "Result" => {
                if !serde_like {
                    bail!("Result cannot be zerocopy-encoded");
                }
                let [ok, err] = generics else {
                    bail!("Result needs two type arguments")
                };
                match take(input, 1)?[0] {
                    0 => Value::Result(Ok(Box::new(
                        self.decode(ok, encoding, input)?,
                    ))),
                    1 => Value::Result(Err(Box::new(
                        self.decode(err, encoding, input)?,
                    ))),
                    tag => bail!("bad Result tag {tag}"),
                }
            }
            other => match self.registry.get(other) {
                Some(TypeDef::Newtype(inner)) => Value::Newtype(
                    other.to_string(),
                    Box::new(self.decode(
                        &TypeExpr::parse(inner)?,
                        encoding,
                        input,
                    )?),
                ),
                Some(TypeDef::Enum(variants)) => {
                    let index = take(input, 1)?[0];
                    let name =
                        variants.get(usize::from(index)).cloned().ok_or_else(
                            || anyhow!("{other} has no variant {index}"),
                        )?;
                    Value::Variant {
                        ty: other.to_string(),
                        index,
                        name,
                    }
                }
                Some(TypeDef::Struct(fields)) => {
                    let mut values = Vec::new();
                    for (field, ty) in fields {
                        values.push((
                            field.clone(),
                            self.decode(
                                &TypeExpr::parse(ty)?,
                                encoding,
                                input,
                            )?,
                        ));
                    }
                    Value::Struct(other.to_string(), values)
                }
                Some(TypeDef::ErrorCodes(_)) => {
                    bail!("{other} is an error code, not a value")
                }
                None => bail!("unknown type {other}; add it to the registry"),
            },
        })
    }
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if input.len() < n {
        bail!("message too short: wanted {n} bytes, {} left", input.len());
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

fn last_segment(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => {
            p.path.segments.last().map(|s| s.ident.to_string())
        }
        _ => None,
    }
}

/// Mirrors `abi::extract_new_generation` without depending on `abi`.
fn abi_dead_generation(code: u32) -> Option<u8> {
    const FIRST_DEAD_CODE: u32 = 0xffff_ff00;
    (code & FIRST_DEAD_CODE == FIRST_DEAD_CODE).then_some(code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor() -> Decoder {
        Decoder::load(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../idl/sensor.idol"
        )))
        .unwrap()
    }

    #[test]
    fn decodes_a_post() {
        let d = sensor();
        // post(id: SensorId(3), value: 31.5, timestamp: 1000), hubpack-encoded
        let mut body = 3u32.to_le_bytes().to_vec();
        body.extend(31.5f32.to_le_bytes());
        body.extend(1000u64.to_le_bytes());
        let op =
            d.ops.iter().position(|o| o.name == "post").unwrap() as u16 + 1;
        assert_eq!(
            d.describe_request(op, &body),
            "post(id: SensorId(3), value: 31.5, timestamp: 1000)"
        );
        assert_eq!(d.describe_reply(op, 0, &[]), "()");
    }

    #[test]
    fn decodes_get_replies() {
        let d = sensor();
        let get =
            d.ops.iter().position(|o| o.name == "get").unwrap() as u16 + 1;
        assert_eq!(
            d.describe_reply(get, 0, &31.5f32.to_le_bytes()),
            "Ok(31.5)"
        );
        assert_eq!(
            d.describe_reply(get, 2, &[]),
            "Err(SensorError::NoReading)"
        );
        let raw = d
            .ops
            .iter()
            .position(|o| o.name == "get_raw_reading")
            .unwrap() as u16
            + 1;
        let mut body = vec![1u8, 1, 2];
        body.extend(7u64.to_le_bytes());
        assert_eq!(
            d.describe_reply(raw, 0, &body),
            "Some((Err(NoData::DeviceNotPresent), 7))"
        );
        let reading =
            d.ops.iter().position(|o| o.name == "get_reading").unwrap() as u16
                + 1;
        let mut body = 5u64.to_le_bytes().to_vec();
        body.extend(2.0f32.to_le_bytes());
        assert_eq!(
            d.describe_reply(reading, 0, &body),
            "Ok(Reading { timestamp: 5, value: 2 })"
        );
    }
}
