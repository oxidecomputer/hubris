// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use proc_macro2::TokenStream;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    idol::server::build_server_support(
        "../../idl/net.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let global_config = build_util::config::<GlobalConfig>()?;

    generate_net_config(&global_config.net)?;
    build_util::expose_target_board();

    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// Network config schema definition.
//

#[derive(Deserialize)]
struct GlobalConfig {
    net: NetConfig,
}

#[derive(Deserialize)]
struct NetConfig {
    sockets: BTreeMap<String, SocketConfig>,
}

/// TODO: this type really wants to be an enum, but the toml crate's enum
/// handling is really, really fragile, and currently it would be an enum with a
/// single variant anyway.
#[derive(Deserialize)]
struct SocketConfig {
    kind: String,
    owner: TaskNote,
    port: u16,
    tx: BufSize,
    rx: BufSize,
}

#[derive(Deserialize)]
struct BufSize {
    packets: usize,
    bytes: usize,
}

#[derive(Deserialize)]
struct TaskNote {
    name: String,
    notification: u32,
}

///////////////////////////////////////////////////////////////////////////////
// Net config code generation.

fn generate_net_config(
    config: &NetConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("net_config.rs");

    let mut out = std::fs::File::create(&dest_path)?;

    let socket_count = config.sockets.len();
    writeln!(
        out,
        "{}",
        quote::quote! {
            use core::sync::atomic::{AtomicBool, Ordering};
            use smoltcp::socket::{UdpPacketMetadata, UdpSocket, UdpSocketBuffer};

            pub const SOCKET_COUNT: usize = #socket_count;
        }
    )?;

    for (name, socket) in &config.sockets {
        writeln!(out, "{}", generate_socket_state(name, socket)?)?;
    }
    writeln!(out, "{}", generate_state_struct(&config)?)?;
    writeln!(out, "{}", generate_constructor(&config)?)?;
    writeln!(out, "{}", generate_owner_info(&config)?)?;
    writeln!(out, "{}", generate_port_table(&config)?)?;

    drop(out);

    //call_rustfmt::rustfmt(&dest_path)?;

    Ok(())
}

fn generate_port_table(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let consts = config.sockets.values().map(|socket| {
        let port = socket.port;
        quote::quote! { #port }
    });

    let n = config.sockets.len();

    Ok(quote::quote! {
        pub(crate) const SOCKET_PORTS: [u16; #n] = [
            #( #consts ),*
        ];
    })
}

fn generate_owner_info(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let consts = config.sockets.values().map(|socket| {
        let task: syn::Ident = syn::parse_str(&socket.owner.name).unwrap();
        let note = socket.owner.notification;
        quote::quote! {
            (
                userlib::TaskId::for_index_and_gen(
                    hubris_num_tasks::Task::#task as usize,
                    userlib::Generation::ZERO,
                ),
                #note,
            )
        }
    });

    let n = config.sockets.len();

    Ok(quote::quote! {
        pub(crate) const SOCKET_OWNERS: [(userlib::TaskId, u32); #n] = [
            #( #consts ),*
        ];
    })
}

fn generate_socket_state(
    name: &str,
    config: &SocketConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    if config.kind != "udp" {
        return Err("unsupported socket kind".into());
    }

    let tx = generate_buffers(name, "TX", &config.tx)?;
    let rx = generate_buffers(name, "RX", &config.rx)?;
    Ok(quote::quote! {
        #tx
        #rx
    })
}

fn generate_buffers(
    name: &str,
    dir: &str,
    config: &BufSize,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let pktcnt = config.packets;
    let bytecnt = config.bytes;
    let upname = name.to_ascii_uppercase();
    let hdrname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_HDR_{}", dir, upname)).unwrap();
    let bufname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_DAT_{}", dir, upname)).unwrap();
    Ok(quote::quote! {
        static mut #hdrname: [UdpPacketMetadata; #pktcnt] = [
            UdpPacketMetadata::EMPTY; #pktcnt
        ];
        static mut #bufname: [u8; #bytecnt] = [0u8; #bytecnt];
    })
}

fn generate_state_struct(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let n = config.sockets.len();
    Ok(quote::quote! {
        pub(crate) struct Sockets<'a>(pub [UdpSocket<'a>; #n]);
    })
}

fn generate_constructor(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let sockets = config.sockets.keys().map(|name| {
        let upname = name.to_ascii_uppercase();
        let rxhdrs: syn::Ident =
            syn::parse_str(&format!("SOCK_RX_HDR_{}", upname)).unwrap();
        let rxbytes: syn::Ident =
            syn::parse_str(&format!("SOCK_RX_DAT_{}", upname)).unwrap();
        let txhdrs: syn::Ident =
            syn::parse_str(&format!("SOCK_TX_HDR_{}", upname)).unwrap();
        let txbytes: syn::Ident =
            syn::parse_str(&format!("SOCK_TX_DAT_{}", upname)).unwrap();

        quote::quote! {
            UdpSocket::new(
                UdpSocketBuffer::new(
                    unsafe { &mut #rxhdrs[..] },
                    unsafe { &mut #rxbytes[..] },
                ),
                UdpSocketBuffer::new(
                    unsafe { &mut #txhdrs[..] },
                    unsafe { &mut #txbytes[..] },
                ),
            )
        }
    });
    Ok(quote::quote! {
        static CTOR_FLAG: AtomicBool = AtomicBool::new(false);
        pub(crate) fn construct_sockets() -> Sockets<'static> {
            let second_time = CTOR_FLAG.swap(true, Ordering::Relaxed);
            if second_time { panic!() }

            // Now that we're confident we're not aliasing, we can touch these
            // static muts.
            Sockets([
                #( #sockets ),*
            ])
        }
    })
}
