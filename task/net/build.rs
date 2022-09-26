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
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("net_config.rs");

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

    if build_util::has_feature("vlan") {
        build_net::generate_vlan_consts(config, &mut out)?;
    }

    for (name, socket) in &config.sockets {
        writeln!(
            out,
            "{}",
            generate_socket_state(
                name,
                socket,
                config.vlan.map(|v| v.count).unwrap_or(1)
            )?
        )?;
    }
    writeln!(out, "{}", generate_state_struct(config))?;
    writeln!(out, "{}", generate_constructor(config)?)?;
    writeln!(out, "{}", generate_owner_info(config)?)?;
    writeln!(out, "{}", generate_port_table(config)?)?;

    build_net::generate_socket_enum(config, &mut out)?;

    // Sanity-checking to match the fru-id-eeprom feature with the fru-id-bus
    // configuration variable.
    match (config.fru_id_bus.is_some(), cfg!(feature = "fru-id-eeprom")) {
        (true, false) => panic!(
            "fru-id-eeprom feature must be enabled when fru-id-bus is provided"
        ),
        (false, true) => panic!(
            "fru-id-bus must be provided when fru-id-eeprom feature is enabled"
        ),
        _ => (),
    }
    #[cfg(feature = "fru-id-eeprom")]
    {
        let fru_id_bus = config.fru_id_bus.as_ref().unwrap();
        writeln!(out, "{}", generate_fru_id_stub(fru_id_bus)?)?;
        build_i2c::codegen(build_i2c::Disposition::Devices)?;
    }

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
    vlan_count: usize,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    if config.kind != "udp" {
        return Err("unsupported socket kind".into());
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
    let hdrname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_HDR_{}", dir, upname)).unwrap();
    let bufname: syn::Ident =
        syn::parse_str(&format!("SOCK_{}_DAT_{}", dir, upname)).unwrap();
    quote::quote! {
        static mut #hdrname: [[UdpPacketMetadata; #pktcnt]; #vlan_count] = [
            [UdpPacketMetadata::EMPTY; #pktcnt]; #vlan_count
        ];
        static mut #bufname: [[u8; #bytecnt]; #vlan_count] = [[0u8; #bytecnt]; #vlan_count];
    }
}

fn generate_state_struct(config: &NetConfig) -> TokenStream {
    let n = config.sockets.len();
    quote::quote! {
        pub(crate) struct Sockets<'a, const N: usize>(pub [[UdpSocket<'a>; #n]; N]);
    }
}

fn generate_constructor(
    config: &NetConfig,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let name_to_sockets = |name: &String, i: usize| {
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
                    unsafe { &mut #rxhdrs[#i][..] },
                    unsafe { &mut #rxbytes[#i][..] },
                ),
                UdpSocketBuffer::new(
                    unsafe { &mut #txhdrs[#i][..] },
                    unsafe { &mut #txbytes[#i][..] },
                ),
            )
        }
    };
    let vlan_count = config.vlan.map(|v| v.count).unwrap_or(1);
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

#[cfg(feature = "fru-id-eeprom")]
fn generate_fru_id_stub(
    fru_id_bus: &str,
) -> Result<TokenStream, Box<dyn std::error::Error>> {
    let function_name: proc_macro2::TokenStream =
        format!("at24csw080_{fru_id_bus}").parse().unwrap();
    let out = quote::quote! {
        include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
        pub fn get_fru_id_eeprom(i2c_task: userlib::TaskId)
            -> drv_i2c_devices::at24csw080::At24Csw080
        {
            let devs = i2c_config::devices::#function_name(i2c_task);
            assert_eq!(devs.len(), 1);
            drv_i2c_devices::at24csw080::At24Csw080::new(devs[0])
        }
    };
    Ok(out)
}
