use std::collections::BTreeMap;
use std::path::PathBuf;
use indexmap::IndexMap;

use knuffel::traits::{Decode, DecodeScalar, ErrorSpan};
use knuffel::ast::{Literal, SpannedNode, TypeName};
use knuffel::decode::Context;
use knuffel::errors::DecodeError;
use knuffel::span::Spanned;

#[derive(Clone, Debug, knuffel::Decode)]
#[knuffel(span_type = knuffel::span::Span)]
pub struct AppDef {
    #[knuffel(child, unwrap(argument))]
    pub app_name: String,
    #[knuffel(child, unwrap(argument))]
    pub target: String,
    #[knuffel(child, unwrap(argument))]
    pub board: String,
    #[knuffel(child, unwrap(argument))]
    pub chip: PathBuf,
    #[knuffel(child, unwrap(argument))]
    pub stack_size: Option<u32>,
    #[knuffel(child)]
    pub secure_separation: bool,

    #[knuffel(child)]
    pub kernel: KernelSection,
    #[knuffel(children(name = "task"))]
    pub tasks: Vec<TaskSection>,
    #[knuffel(children(name = "output"))]
    pub outputs: Vec<Output>,
    #[knuffel(child)]
    pub config: Config,
    #[knuffel(children(name = "signing"))]
    pub signing: Vec<SigningSection>,
    #[knuffel(child)]
    pub bootloader: Option<BootloaderSection>,
}

#[derive(Clone, Debug, knuffel::Decode)]
pub struct KernelSection {
    #[knuffel(child, unwrap(argument))]
    pub crate_name: String,
    #[knuffel(child, default, unwrap(arguments))]
    pub features: Vec<String>,
    #[knuffel(child, unwrap(properties))]
    pub requires: IndexMap<String, u32>,
    #[knuffel(child, unwrap(argument))]
    pub stack_size: Option<u32>,
}

#[derive(Clone, Debug, knuffel::Decode)]
#[knuffel(span_type = knuffel::span::Span)]
pub struct TaskSection {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(child, unwrap(argument))]
    pub crate_name: String,
    #[knuffel(child, default, unwrap(arguments))]
    pub features: Vec<String>,
    #[knuffel(child, unwrap(argument))]
    pub priority: u8,
    #[knuffel(child, unwrap(properties))]
    pub max_sizes: IndexMap<String, u32>,
    #[knuffel(child)]
    pub start: bool,
    #[knuffel(child, unwrap(argument))]
    pub stack_size: Option<u32>,
    #[knuffel(child, unwrap(arguments), default)]
    pub uses: Vec<String>,
    #[knuffel(child, default)]
    pub task_slots: TaskSlots,
    #[knuffel(child, default)]
    pub notify: Notify,
    #[knuffel(child, default, unwrap(properties))]
    pub sections: IndexMap<String, String>,

    #[knuffel(child, default)]
    pub config: Config,
}

#[derive(Clone, Debug, Default, knuffel::Decode)]
#[knuffel(span_type = knuffel::span::Span)]
pub struct Config(#[knuffel(children)] Vec<XNode>);

impl Config {
    pub fn iter(&self) -> impl Iterator<Item = &kdl::KdlNode> {
        self.0.iter().map(|xn| &xn.0)
    }

    pub fn into_doc(self) -> Option<kdl::KdlDocument> {
        if self.0.is_empty() {
            None
        } else {
            let mut doc = kdl::KdlDocument::new();
            for XNode(node) in self.0 {
                doc.nodes_mut().push(node);
            }
            Some(doc)
        }
    }
}

#[derive(Clone, Debug, knuffel::Decode)]
#[knuffel(span_type = knuffel::span::Span)]
pub struct BootloaderSection {
    #[knuffel(child, unwrap(argument))]
    pub crate_name: String,
    #[knuffel(child, default, unwrap(arguments))]
    pub features: Vec<String>,
    #[knuffel(child, default, unwrap(properties))]
    pub sections: IndexMap<String, String>,
    #[knuffel(child, default, unwrap(argument))]
    pub imagea_flash_start: u32,
    #[knuffel(child, default, unwrap(argument))]
    pub imagea_flash_size: u32,
    #[knuffel(child, default, unwrap(argument))]
    pub imagea_ram_start: u32,
    #[knuffel(child, default, unwrap(argument))]
    pub imagea_ram_size: u32,
}

#[derive(Clone, Debug, Default, knuffel::Decode)]
pub struct Notify {
    #[knuffel(children(name = "irq"))]
    pub irqs: Vec<Interrupt>,
}

#[derive(Clone, Debug, Default, knuffel::Decode)]
pub struct Interrupt {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(property)]
    pub mask: u32,
}

#[derive(Clone, Debug, Default, knuffel::Decode)]
pub struct TaskSlots {
    #[knuffel(arguments)]
    simple: Vec<String>,
    #[knuffel(properties)]
    explicit: BTreeMap<String, String>,
}

impl TaskSlots {
    pub fn callees(&self) -> impl Iterator<Item = &str> {
        self.simple.iter()
            .chain(self.explicit.values())
            .map(|s| s.as_str())
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        if let Some(n) = self.simple.iter().find(|&x| x == name) {
            Some(n)
        } else {
            Some(self.explicit.get(name)?.as_str())
        }
    }
}

#[derive(Clone, Debug, knuffel::Decode)]
pub struct Output {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(child, unwrap(argument))]
    pub address: u32,
    #[knuffel(child, unwrap(argument))]
    pub size: u32,
    #[knuffel(child)]
    pub read: bool,
    #[knuffel(child)]
    pub write: bool,
    #[knuffel(child)]
    pub execute: bool,
    #[knuffel(child)]
    pub dma: bool,
}

#[derive(Clone, Debug, knuffel::Decode)]
pub struct SigningSection {
    #[knuffel(argument)]
    pub name: String,
    #[knuffel(child, unwrap(argument))]
    pub method: SigningMethod,
    #[knuffel(child, unwrap(argument))]
    pub priv_key: Option<PathBuf>,
    #[knuffel(child, unwrap(argument))]
    pub root_cert: Option<PathBuf>,
}


#[derive(Copy, Clone, Debug, knuffel::DecodeScalar)]
pub enum SigningMethod {
    Crc,
    Rsa,
    Ecc,
}

impl std::fmt::Display for SigningMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Crc => "crc",
            Self::Rsa => "rsa",
            Self::Ecc => "ecc",
        })
    }
}

#[derive(Clone, Debug)]
pub struct XNode(pub kdl::KdlNode);

impl<S: ErrorSpan> Decode<S> for XNode {
    fn decode_node(
        in_node: &SpannedNode<S>, 
        ctx: &mut Context<S>
    ) -> Result<Self, DecodeError<S>> {
        let in_node = &**in_node;

        let mut out_node = kdl::KdlNode::new(in_node.node_name.to_string());
        if let Some(tn) = &in_node.type_name {
            out_node.set_ty(tn.as_str().to_string());
        }

        for arg in &in_node.arguments {
            let XValue(v) = DecodeScalar::decode(arg, ctx)?;
            out_node.push(v);
        }

        for (name, value) in &in_node.properties {
            let XValue(v) = DecodeScalar::decode(value, ctx)?;
            out_node.push((name.to_string(), v));
        }

        if let Some(children) = &in_node.children {
            let out_children = out_node.ensure_children();
            for kid in &**children {
                let XNode(out_kid) = Decode::decode_node(
                    kid,
                    ctx,
                )?;
                out_children.nodes_mut().push(out_kid);
            }
        }

        Ok(XNode(out_node))
    }
}

pub struct XValue(pub kdl::KdlValue);

impl<S: ErrorSpan> DecodeScalar<S> for XValue {
    fn type_check(
        _type_name: &Option<Spanned<TypeName, S>>, 
        _ctx: &mut Context<S>
    ) {
        // uh
    }

    fn raw_decode(
        value: &Spanned<Literal, S>, 
        _ctx: &mut Context<S>
    ) -> Result<Self, DecodeError<S>> {
        let v = match &**value {
            Literal::Null => kdl::KdlValue::Null,
            Literal::Bool(b) => kdl::KdlValue::Bool(*b),
            Literal::Int(i) => kdl::KdlValue::Base10(i64::try_from(i).unwrap()),
            Literal::Decimal(_) => {
                panic!("Sorry, knuffel doesn't appear to give any \
                    way to access the actual value of a decimal number.");
            }
            Literal::String(s) => kdl::KdlValue::String(s.to_string()),
        };
        Ok(XValue(v))
    }
}

