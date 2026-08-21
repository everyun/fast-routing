//! KiCad 10 round-trip Specctra SES integration test.

use fr_core::RoutingJob;
use std::process::Command;

#[test]
fn test_kicad_ses_import_roundtrip() {
    let dsn_path = "upstream-freerouting/examples/tutorial_board/tutorial_board.dsn";
    if let Ok(dsn_content) = std::fs::read_to_string(dsn_path) {
        let job = RoutingJob::new(&dsn_content);
        let result = job.execute().unwrap();
        assert!(result.ses_content.contains("(session "));
        assert!(result.ses_content.contains("(base_design "));
        assert!(result.ses_content.contains("(placement"));
        assert!(result.ses_content.contains("(was_is"));
        assert!(result.ses_content.contains("(routes"));

        // If KiCad python is available, test real pcbnew.ImportSpecctraSES
        let kicad_python = "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3";
        if std::path::Path::new(kicad_python).exists() {
            let temp_ses = "/tmp/tutorial_board_test.ses";
            std::fs::write(temp_ses, &result.ses_content).unwrap();

            let output = Command::new(kicad_python)
                .arg("-c")
                .arg(format!(
                    "import pcbnew; board = pcbnew.LoadBoard('upstream-freerouting/examples/tutorial_board/tutorial_board.kicad_pcb'); res = pcbnew.ImportSpecctraSES(board, '{}'); assert res == True, f'ImportSpecctraSES returned {{res}}'",
                    temp_ses
                ))
                .output();

            if let Ok(out) = output {
                assert!(out.status.success(), "KiCad pcbnew.ImportSpecctraSES failed with exit code {:?}, stderr: {:?}", out.status.code(), String::from_utf8_lossy(&out.stderr));
            }
        }
    }
}
