#![forbid(unsafe_code)]
//! CLI front end for the differential harness. See `vaco_checkasm` (the
//! library half of this crate) for the `Kernel`/`Differential` API this binds
//! together.
//!
//! ```text
//! vaco-checkasm verify        # run every wired-in kernel, exit non-zero on any mismatch
//! vaco-checkasm list          # print the wired-in kernel names
//! ```
//!
//! There is no plugin registry here: a kernel family becomes reachable from
//! this binary by adding one `Kernel` impl under `src/kernels` and one line
//! in [`run_all`]. A crate that wants its own kernels checked without a
//! change to this binary can depend on the `vaco_checkasm` library directly
//! and call `Differential::<K>::run()` from its own tests — that is how
//! `vaco-checkasm`'s own `kernels::scale_affine` module is itself tested.

use vaco_checkasm::kernels::fir_mc::FirMcKernel;
use vaco_checkasm::kernels::masked_select::MaskedSelectKernel;
use vaco_checkasm::kernels::scale_affine::AffineRowKernel;
use vaco_checkasm::{Differential, Kernel, Report};

const USAGE: &str = "usage: vaco-checkasm [verify|list]";

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        None | Some("verify") => run_all(),
        Some("list") => {
            list_all();
            0
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("unknown subcommand '{other}'");
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// One entry in the wired-in kernel table: a name (for `list`) and a thunk
/// that runs the differential and prints its report (for `verify`).
///
/// A plain struct of function pointers rather than `dyn Kernel` — `Kernel`
/// is not object-safe (associated types), so the type erasure has to happen
/// one level up, at the point each kernel is already monomorphised.
struct Entry {
    name: &'static str,
    verify: fn() -> bool,
}

fn verify_report<K: Kernel>() -> bool
where
    K::Lane: PartialEq,
{
    let report: Report<K> = Differential::<K>::run();
    print!("{report}");
    report.is_clean()
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: AffineRowKernel::NAME,
        verify: verify_report::<AffineRowKernel>,
    },
    Entry {
        name: MaskedSelectKernel::NAME,
        verify: verify_report::<MaskedSelectKernel>,
    },
    Entry {
        name: FirMcKernel::NAME,
        verify: verify_report::<FirMcKernel>,
    },
];

fn run_all() -> i32 {
    if ENTRIES.is_empty() {
        eprintln!("no kernels wired in");
        return 1;
    }
    let mut ok = true;
    for entry in ENTRIES {
        ok &= (entry.verify)();
    }
    i32::from(!ok)
}

fn list_all() {
    for entry in ENTRIES {
        println!("{}", entry.name);
    }
}
