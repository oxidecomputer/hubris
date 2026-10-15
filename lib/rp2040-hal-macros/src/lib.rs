extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{Item, ItemFn, Stmt, parse, parse_macro_input};

#[proc_macro_attribute]
pub fn entry(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut f = parse_macro_input!(input as ItemFn);

    if !args.is_empty() {
        return parse::Error::new(
            Span::call_site(),
            "This attribute accepts no arguments",
        )
        .to_compile_error()
        .into();
    }

    let clear_locks: TokenStream = quote!(unsafe {
        const SIO_BASE: u32 = 0xd0000000;
        const SPINLOCK0_PTR: *mut u32 = (SIO_BASE + 0x100) as *mut u32;
        const SPINLOCK_COUNT: usize = 32;
        for i in 0..SPINLOCK_COUNT {
            SPINLOCK0_PTR.wrapping_add(i).write_volatile(1);
        }
    })
    .into();
    let clear_locks = parse_macro_input!(clear_locks as Stmt);

    // statics must stay first so cortex_m_rt::entry still finds them
    let stmts = insert_after_static(f.block.stmts, clear_locks);
    f.block.stmts = stmts;

    quote!(
        #[::cortex_m_rt::entry]
        #f
    )
    .into()
}

/// Insert new statements after initial block of statics
fn insert_after_static(
    stmts: impl IntoIterator<Item = Stmt>,
    insert: Stmt,
) -> Vec<Stmt> {
    let mut istmts = stmts.into_iter();
    let mut stmts = vec![];
    for stmt in istmts.by_ref() {
        match stmt {
            Stmt::Item(Item::Static(var)) => {
                stmts.push(Stmt::Item(Item::Static(var)));
            }
            _ => {
                stmts.push(insert);
                stmts.push(stmt);
                break;
            }
        }
    }
    stmts.extend(istmts);

    stmts
}
