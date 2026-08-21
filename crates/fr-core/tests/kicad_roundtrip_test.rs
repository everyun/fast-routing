//! KiCad 10 round-trip Specctra SES integration test.

use fr_core::RoutingJob;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_kicad_python() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3".to_string(),
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3".to_string(),
        format!("{}/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3", home),
        format!("{}/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3", home),
        "python3".to_string(),
    ];

    for candidate in &candidates {
        if let Ok(out) = Command::new(candidate).arg("-c").arg("import pcbnew; print('ok')").output() {
            if out.status.success() {
                return Some(candidate.clone());
            }
        }
    }
    None
}

fn get_repo_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn test_kicad_ses_import_roundtrip() {
    let repo_root = get_repo_root();
    let test_boards = [
        (
            "upstream-freerouting/fixtures/Issue069-TestSensel/TestSensel.dsn",
            "upstream-freerouting/fixtures/Issue069-TestSensel/TestSensel.kicad_pcb",
        ),
        (
            "upstream-freerouting/examples/tutorial_board/tutorial_board.dsn",
            "upstream-freerouting/examples/tutorial_board/tutorial_board.kicad_pcb",
        ),
    ];

    let kicad_python = find_kicad_python();

    for (rel_dsn, rel_pcb) in test_boards {
        let dsn_path = repo_root.join(rel_dsn);
        let pcb_path = repo_root.join(rel_pcb);

        assert!(dsn_path.exists(), "Required fixture {:?} must exist", dsn_path);
        let dsn_content = std::fs::read_to_string(&dsn_path).expect("failed to read dsn fixture");
        let job = RoutingJob::new(&dsn_content);
        let result = job.execute().expect("routing job execution failed");

        assert!(result.ses_content.contains("(session "));
        assert!(result.ses_content.contains("(base_design "));
        assert!(result.ses_content.contains("(placement"));
        assert!(result.ses_content.contains("(was_is"));
        assert!(result.ses_content.contains("(routes"));

        if let Some(ref py_bin) = kicad_python {
            let temp_ses = format!("/tmp/roundtrip_test_{}.ses", pcb_path.file_stem().unwrap().to_str().unwrap());
            std::fs::write(&temp_ses, &result.ses_content).unwrap();

            let script = format!(
                "import pcbnew; board = pcbnew.LoadBoard('{}'); res = pcbnew.ImportSpecctraSES(board, '{}'); assert res == True, f'ImportSpecctraSES failed with {{res}}'",
                pcb_path.display(), temp_ses
            );

            let output = Command::new(py_bin)
                .arg("-c")
                .arg(script)
                .output()
                .expect("failed to execute kicad python script");

            assert!(
                output.status.success(),
                "KiCad pcbnew.ImportSpecctraSES failed for {}! stderr: {}",
                pcb_path.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
