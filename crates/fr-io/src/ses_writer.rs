//! Specctra SES (Session) file writer for exporting routed PCB designs.
//!
//! Strictly adheres to the Specctra SES specification and upstream Freerouting
//! `SesWriter.java` format to ensure seamless round-trip import into KiCad 10,
//! LibrePCB, and other EDA tools (`pcbnew.ImportSpecctraSES`).

use crate::parser::{DsnComponent, DsnPackage, DsnPadstack, DsnVia, DsnWire};
use std::collections::HashSet;
use std::fmt::Write;

/// Generates a Specctra `.ses` session file string.
pub struct SesWriter {
    pub pcb_name: String,
    pub unit: String,
    pub resolution: f64,
}

impl SesWriter {
    pub fn new(pcb_name: &str) -> Self {
        SesWriter {
            pcb_name: pcb_name.to_string(),
            unit: "um".to_string(),
            resolution: 10.0,
        }
    }

    pub fn with_resolution(pcb_name: &str, unit: &str, resolution: f64) -> Self {
        SesWriter {
            pcb_name: pcb_name.to_string(),
            unit: unit.to_string(),
            resolution,
        }
    }

    /// Generates the complete Specctra SES session file content.
    pub fn write_full_session(
        &self,
        components: &[DsnComponent],
        packages: &[DsnPackage],
        padstacks: &[DsnPadstack],
        wires: &[DsnWire],
        vias: &[DsnVia],
        net_names: &[String],
    ) -> String {
        let mut out = String::new();

        // Extract clean base filename without path or duplicate extensions
        let raw_filename = std::path::Path::new(&self.pcb_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.pcb_name);

        let clean_base = if let Some(stripped) = raw_filename.strip_suffix(".dsn") {
            stripped
        } else if let Some(stripped) = raw_filename.strip_suffix(".ses") {
            stripped
        } else {
            raw_filename
        };

        let session_name = format!("{}.ses", clean_base);
        let base_design_name = format!("{}.dsn", clean_base);

        writeln!(out, "(session \"{}\"", session_name).unwrap();
        writeln!(out, "  (base_design \"{}\")", base_design_name).unwrap();

        // 1. Placement scope
        writeln!(out, "  (placement").unwrap();
        writeln!(out, "    (resolution {} {})", self.unit, self.resolution as i64).unwrap();
        for pkg in packages {
            let placed_comps: Vec<&DsnComponent> = components
                .iter()
                .filter(|c| c.package_name == pkg.name)
                .collect();
            if !placed_comps.is_empty() {
                writeln!(out, "    (component \"{}\"", pkg.name).unwrap();
                for comp in placed_comps {
                    let rot_str = if comp.rotation.fract() == 0.0 {
                        format!("{:.0}", comp.rotation)
                    } else {
                        format!("{:.3}", comp.rotation)
                    };
                    writeln!(
                        out,
                        "      (place \"{}\" {} {} {} {})",
                        comp.name,
                        (comp.x * self.resolution).round() as i64,
                        (comp.y * self.resolution).round() as i64,
                        comp.side,
                        rot_str
                    )
                    .unwrap();
                }
                writeln!(out, "    )").unwrap();
            }
        }
        writeln!(out, "  )").unwrap();

        // 2. Was-is scope
        writeln!(out, "  (was_is").unwrap();
        writeln!(out, "  )").unwrap();

        // 3. Routes scope
        writeln!(out, "  (routes").unwrap();
        writeln!(out, "    (resolution {} {})", self.unit, self.resolution as i64).unwrap();
        writeln!(out, "    (parser").unwrap();
        writeln!(out, "      (string_quote \")").unwrap();
        writeln!(out, "      (host_cad \"Freerouting-Rust\")").unwrap();
        writeln!(out, "      (host_version \"0.1.0\")").unwrap();
        writeln!(out, "    )").unwrap();

        // 4. Library out scope (via padstacks only)
        writeln!(out, "    (library_out").unwrap();
        let mut written_padstacks = HashSet::new();
        for padstack in padstacks {
            if padstack.name.to_ascii_lowercase().contains("via") && written_padstacks.insert(padstack.name.clone()) {
                writeln!(out, "      (padstack \"{}\"", padstack.name).unwrap();
                for shape in &padstack.shapes {
                    write!(out, "        (shape ({} {}", shape.shape_type, shape.layer).unwrap();
                    for dim in &shape.dimensions {
                        let dim_val = if dim.fract() == 0.0 {
                            format!("{:.0}", dim)
                        } else {
                            format!("{:.4}", dim).trim_end_matches('0').trim_end_matches('.').to_string()
                        };
                        write!(out, " {}", dim_val).unwrap();
                    }
                    writeln!(out, "))").unwrap();
                }
                writeln!(out, "        (attach off)").unwrap();
                writeln!(out, "      )").unwrap();
            }
        }
        writeln!(out, "    )").unwrap();

        // 5. Network out scope (wires & vias per net)
        writeln!(out, "    (network_out").unwrap();
        for net_name in net_names {
            let net_wires: Vec<&DsnWire> = wires.iter().filter(|w| &w.net_name == net_name).collect();
            let net_vias: Vec<&DsnVia> = vias.iter().filter(|v| &v.net_name == net_name).collect();

            if net_wires.is_empty() && net_vias.is_empty() {
                continue;
            }

            writeln!(out, "      (net \"{}\"", net_name).unwrap();

            for wire in net_wires {
                let w_val = (wire.width * self.resolution).round() as i64;
                write!(out, "        (wire (path {} {}", wire.layer, w_val).unwrap();
                for pt in &wire.points {
                    write!(
                        out,
                        " {} {}",
                        (pt.x * self.resolution).round() as i64,
                        (pt.y * self.resolution).round() as i64
                    )
                    .unwrap();
                }
                write!(out, ")").unwrap();
                if let Some(ref fix_type) = wire.fixed_type {
                    if fix_type == "protect" || fix_type == "fix" {
                        write!(out, " (type {})", fix_type).unwrap();
                    }
                }
                writeln!(out, ")").unwrap();
            }

            for via in net_vias {
                write!(
                    out,
                    "        (via \"{}\" {} {}",
                    via.padstack_name,
                    (via.x * self.resolution).round() as i64,
                    (via.y * self.resolution).round() as i64
                )
                .unwrap();
                if let Some(ref fix_type) = via.fixed_type {
                    if fix_type == "protect" || fix_type == "fix" {
                        write!(out, " (type {})", fix_type).unwrap();
                    }
                }
                writeln!(out, ")").unwrap();
            }

            writeln!(out, "      )").unwrap();
        }
        writeln!(out, "    )").unwrap();

        writeln!(out, "  )").unwrap();
        writeln!(out, ")").unwrap();

        out
    }

    /// Legacy convenience wrapper.
    pub fn write_session(
        &self,
        wires: &[DsnWire],
        vias: &[DsnVia],
        net_names: &[String],
    ) -> String {
        self.write_full_session(&[], &[], &[], wires, vias, net_names)
    }
}
