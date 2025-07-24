// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, bail, Result};
use build_net::{BufSize, NetConfig, SocketConfig};
use proc_macro2::TokenStream;
use std::io::Write;

fn main() -> Result<()> {
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/net.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )
        .map_err(|e| anyhow!(e))?;

    let net_config = build_net::load_net_config()?;

    generate_net_config(&net_config)?;
    build_util::expose_target_board();
    build_util::build_notifications()?;

    Ok(())
}

fn generate_net_config(config: &NetConfig) -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("net_config.rs");

    let mut out = std::fs::File::create(dest_path)?;

    let socket_count = config.sockets.len();
    writeln!(
        out,
        "{}",
        quote::quote! {
            use core::sync::atomic::{AtomicBool, Ordering};
            use smoltcp::socket::udp;

            pub const SOCKET_COUNT: usize = #socket_count;
        }
    )?;

    if build_util::has_feature("vlan") {
        build_net::generate_vlan_consts(config, &mut out)?;
    }

    for (name, socket) in &config.sockets {
        writeln!(
            out,
            "{}",
            generate_socket_state(name, socket, config.vlans.len().max(1))?
        )?;
    }
    writeln!(out, "{}", generate_state_struct(config))?;
    writeln!(out, "{}", generate_constructor(config)?)?;
    writeln!(out, "{}", generate_owner_info(config)?)?;
    writeln!(out, "{}", generate_port_table(config)?)?;

    build_net::generate_port_consts(config, &mut out)?;
    build_net::generate_socket_enum(config, &mut out)?;

    drop(out);

    //call_rustfmt::rustfmt(&dest_path)?;

    Ok(())
}

fn generate_port_table(config: &NetConfig) -> Result<TokenStream> {
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

fn generate_owner_info(config: &NetConfig) -> Result<TokenStream> {
    let consts: Vec<_> = config
        .sockets
        .values()
        .map(|socket| {
            let task: syn::Ident = syn::parse_str(&socket.owner.name).unwrap();
            let note: syn::Ident = syn::parse_str(&format!(
                "{}_MASK",
                socket.owner.notification.to_uppercase().replace('-', "_")
            ))
            .unwrap();
            Ok(quote::quote! {
                (
                    userlib::TaskId::for_index_and_gen(
                        hubris_num_tasks::Task::#task as usize,
                        userlib::Generation::ZERO,
                    ),
                    crate::notifications::#task::#note,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;

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
    vlan_count: usize,
) -> Result<TokenStream> {
    if config.kind != "udp" {
        bail!("unsupported socket kind");
    }

    let tx = generate_buffers(name, "TX", &config.tx, vlan_count);
    let rx = generate_buffers(name, "RX", &config.rx, vlan_count);
    Ok(quote::quote! {
        #tx
        #rx
    })
}

fn generate_buffers(
    name: &str,
    dir: &str,
    config: &BufSize,
    vlan_count: usize,
) -> TokenStream {
    let pktcnt = config.packets;
    let bytecnt = config.bytes;
    let upname = name.to_ascii_uppercase();
    let hdrname: syn::Ident = quote::format_ident!("SOCK_{dir}_HDR_{upname}");
    let bufname: syn::Ident = quote::format_ident!("SOCK_{dir}_DAT_{upname}");
    quote::quote! {
        static mut #hdrname: [[udp::PacketMetadata; #pktcnt]; #vlan_count] = [
            [udp::PacketMetadata::EMPTY; #pktcnt]; #vlan_count
        ];
        static mut #bufname: [[u8; #bytecnt]; #vlan_count] = [[0u8; #bytecnt]; #vlan_count];
    }
}

fn generate_state_struct(config: &NetConfig) -> TokenStream {
    let n = config.sockets.len();
    quote::quote! {
        pub(crate) struct Sockets<'a, const N: usize>(pub [[udp::Socket<'a>; #n]; N]);
    }
}

fn generate_constructor(config: &NetConfig) -> Result<TokenStream> {
    let name_to_sockets = |name: &String, i: usize| {
        let upname = name.to_ascii_uppercase();
        let rxhdrs: syn::Ident = quote::format_ident!("SOCK_RX_HDR_{upname}");
        let rxbytes: syn::Ident = quote::format_ident!("SOCK_RX_DAT_{upname}");
        let txhdrs: syn::Ident = quote::format_ident!("SOCK_TX_HDR_{upname}");
        let txbytes: syn::Ident = quote::format_ident!("SOCK_TX_DAT_{upname}");

        quote::quote! {
            udp::Socket::new(
                udp::PacketBuffer::new(
                    unsafe { &mut #rxhdrs[#i][..] },
                    unsafe { &mut #rxbytes[#i][..] },
                ),
                udp::PacketBuffer::new(
                    unsafe { &mut #txhdrs[#i][..] },
                    unsafe { &mut #txbytes[#i][..] },
                ),
            )
        }
    };
    let vlan_count = config.vlans.len().max(1);
    let sockets = (0..vlan_count)
        .map(|i| {
            let s = config
                .sockets
                .keys()
                .map(|n| name_to_sockets(n, i))
                .collect::<Vec<_>>();
            quote::quote! {
                [
                    #( #s ),*
                ]
            }
        })
        .collect::<Vec<_>>();
    Ok(quote::quote! {
        static CTOR_FLAG: AtomicBool = AtomicBool::new(false);
        pub(crate) fn construct_sockets() -> Sockets<'static, #vlan_count> {
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
