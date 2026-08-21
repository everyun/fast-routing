//! Specctra SES (Session) file writer for exporting routed PCB designs.

use crate::parser::{DsnVia, DsnWire};
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

    /// Generates the complete SES file content from lists of wires and vias.
    pub fn write_session(
        &self,
        wires: &[DsnWire],
        vias: &[DsnVia],
        net_names: &[String],
    ) -> String {
        let mut out = String::new();

        writeln!(out, "(session \"{}\"", self.pcb_name).unwrap();
        writeln!(out, "  (base_design \"{}.dsn\")", self.pcb_name).unwrap();
        writeln!(out, "  (routes").unwrap();
        writeln!(out, "    (resolution {} {})", self.unit, self.resolution as i64).unwrap();
        writeln!(out, "    (parser").unwrap();
        writeln!(out, "      (host_cad \"Freerouting-Rust\")").unwrap();
        writeln!(out, "      (host_version \"0.1.0\")").unwrap();
        writeln!(out, "    )").unwrap();
        writeln!(out, "    (network_out").unwrap();

        for net_name in net_names {
            let net_wires: Vec<&DsnWire> = wires.iter().filter(|w| &w.net_name == net_name).collect();
            let net_vias: Vec<&DsnVia> = vias.iter().filter(|v| &v.net_name == net_name).collect();

            if net_wires.is_empty() && net_vias.is_empty() {
                continue;
            }

            writeln!(out, "      (net \"{}\"", net_name).unwrap();

            for wire in net_wires {
                write!(out, "        (wire (path {} {}", wire.layer, wire.width as i64).unwrap();
                for pt in &wire.points {
                    write!(out, " {} {}", pt.x as i64, pt.y as i64).unwrap();
                }
                writeln!(out, "))").unwrap();
            }

            for via in net_vias {
                writeln!(
                    out,
                    "        (via \"{}\" {} {})",
                    via.padstack_name, via.x as i64, via.y as i64
                )
                .unwrap();
            }

            writeln!(out, "      )").unwrap();
        }

        writeln!(out, "    )").unwrap();
        writeln!(out, "  )").unwrap();
        writeln!(out, ")").unwrap();

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::DsnPoint;

    #[test]
    fn test_write_ses() {
        let writer = SesWriter::new("my_board");
        let wires = vec![DsnWire {
            net_name: "GND".to_string(),
            layer: "F.Cu".to_string(),
            width: 250.0,
            points: vec![
                DsnPoint { x: 1000.0, y: 1000.0 },
                DsnPoint { x: 2000.0, y: 2000.0 },
            ],
        }];
        let vias = vec![DsnVia {
            net_name: "GND".to_string(),
            padstack_name: "Via[0-1]_800:400_um".to_string(),
            x: 2000.0,
            y: 2000.0,
        }];
        let ses = writer.write_session(&wires, &vias, &["GND".to_string()]);
        assert!(ses.contains("(session \"my_board\""));
        assert!(ses.contains("(wire (path F.Cu 250 1000 1000 2000 2000))"));
        assert!(ses.contains("(via \"Via[0-1]_800:400_um\" 2000 2000)"));
    }
}
