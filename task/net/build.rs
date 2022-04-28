// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_net::{BufSize, NetConfig, SocketConfig};
use proc_macro2::TokenStream;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    idol::server::build_server_support(
        "../../idl/net.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    let net_config = build_net::load_net_config()?;

    generate_net_config(&net_config)?;
    build_util::expose_target_board();

    Ok(())
}

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

    #[cfg(feature = "vlan")]
    generate_vlan_consts(&config, &mut out)?;

    for (name, socket) in &config.sockets {
        writeln!(
            out,
            "{}",
            generate_socket_state(name, socket, config.vlan_count)?
        )?;
    }
    writeln!(out, "{}", generate_state_struct(&config)?)?;
    writeln!(out, "{}", generate_constructor(&config)?)?;
    writeln!(out, "{}", generate_owner_info(&config)?)?;
    writeln!(out, "{}", generate_port_table(&config)?)?;

    build_net::generate_socket_enum(&config, &mut out)?;

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
    vlan_count: Option<usize>,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    if config.kind != "udp" {
        return Err("unsupported socket kind".into());
    }

    let tx = generate_buffers(name, "TX", &config.tx, vlan_count)?;
    let rx = generate_buffers(name, "RX", &config.rx, vlan_count)?;
    Ok(quote::quote! {
        #tx
        #rx
    })
}

fn generate_buffers(
    name: &str,
    dir: &str,
    config: &BufSize,
    vlan_count: Option<usize>,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let pktcnt = config.packets;
    let bytecnt = config.bytes;
    let upname = name.to_ascii_uppercase();
    let hdrname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_HDR_{}", dir, upname)).unwrap();
    let bufname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_DAT_{}", dir, upname)).unwrap();
    Ok(if let Some(vlan_count) = vlan_count {
        quote::quote! {
            static mut #hdrname: [[UdpPacketMetadata; #pktcnt]; #vlan_count] = [
                [UdpPacketMetadata::EMPTY; #pktcnt]; #vlan_count
            ];
            static mut #bufname: [[u8; #bytecnt]; #vlan_count] = [[0u8; #bytecnt]; #vlan_count];
        }
    } else {
        quote::quote! {
            static mut #hdrname: [UdpPacketMetadata; #pktcnt] =
                [UdpPacketMetadata::EMPTY; #pktcnt];
            static mut #bufname: [u8; #bytecnt] = [0u8; #bytecnt];
        }
    })
}

fn generate_state_struct(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let n = config.sockets.len();
    Ok(if let Some(vlan_count) = config.vlan_count {
        quote::quote! {
            pub(crate) struct Sockets<'a>(pub [[UdpSocket<'a>; #n]; #vlan_count]);
        }
    } else {
        quote::quote! {
            pub(crate) struct Sockets<'a>(pub [UdpSocket<'a>; #n]);
        }
    })
}

fn generate_constructor(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let name_to_sockets = |name: &String, i: Option<usize>| {
        let upname = name.to_ascii_uppercase();
        let rxhdrs: syn::Ident =
            syn::parse_str(&format!("SOCK_RX_HDR_{}", upname)).unwrap();
        let rxbytes: syn::Ident =
            syn::parse_str(&format!("SOCK_RX_DAT_{}", upname)).unwrap();
        let txhdrs: syn::Ident =
            syn::parse_str(&format!("SOCK_TX_HDR_{}", upname)).unwrap();
        let txbytes: syn::Ident =
            syn::parse_str(&format!("SOCK_TX_DAT_{}", upname)).unwrap();

        if let Some(i) = i {
            quote::quote! {
                UdpSocket::new(
                    UdpSocketBuffer::new(
                    unsafe { &mut #rxhdrs[#i][..] },
                    unsafe { &mut #rxbytes[#i][..] },
                    ),
                    UdpSocketBuffer::new(
                    unsafe { &mut #txhdrs[#i][..] },
                    unsafe { &mut #txbytes[#i][..] },
                    ),
                )
            }
        } else {
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
        }
    };
    let sockets = if let Some(vlan_count) = config.vlan_count {
        (0..vlan_count)
            .map(|i| {
                let s = config
                    .sockets
                    .keys()
                    .map(|n| name_to_sockets(n, Some(i)))
                    .collect::<Vec<_>>();
                quote::quote! {
                    [
                        #( #s ),*
                    ]
                }
            })
            .collect::<Vec<_>>()
    } else {
        config
            .sockets
            .keys()
            .map(|n| name_to_sockets(n, None))
            .collect::<Vec<_>>()
    };
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
