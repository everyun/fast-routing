//! G1.1 gate instrument: emit `fast-routing.golden-dump/1` for one DSN file.
//!
//! A thin CLI over [`fr_io::golden_dump::emit`]; the emitter itself lives in the library so the
//! in-process gate test (`crates/fr-io/tests/golden_gate_test.rs`) runs exactly the same code
//! path. It is NOT the production `fr-cli` emitter that `bench/tools/README.md` asks for.
//!
//! ```sh
//! cargo run -p fr-io --release --example golden_emit -- in.dsn out.json
//! ```
//!
//! Two diagnostics, both off unless the variable is set:
//!
//! | variable | output |
//! |---|---|
//! | `FR_EMIT_PINS` | `PIN <component> <pin> <x> <y>` per pin, in **board** units — the resolved centres (§9.2.1) the schema does not carry |
//! | `FR_EMIT_WARNINGS` | `WARN <message>` per importer warning |

use fr_io::golden_dump;
use fr_io::specctra;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: golden_emit <in.dsn> <out.json>");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1]).expect("read dsn");
    let design = match specctra::import(&bytes) {
        Ok(design) => design,
        Err(error) => {
            eprintln!("import failed: {error}");
            std::process::exit(1);
        }
    };
    let source = std::path::Path::new(&args[1])
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if std::env::var_os("FR_EMIT_PINS").is_some() {
        for component in &design.components {
            let package = &design.packages[component.package_index];
            for (index, pin) in package.pins.iter().enumerate() {
                let center = component.pin_centers[index];
                eprintln!(
                    "PIN {} {} {} {}",
                    component.name, pin.name, center.x, center.y
                );
            }
        }
    }
    if std::env::var_os("FR_EMIT_WARNINGS").is_some() {
        for warning in &design.warnings {
            eprintln!("WARN {warning}");
        }
    }
    std::fs::write(&args[2], golden_dump::emit(&design, &source)).expect("write json");
}
