//! KiCad 10 round-trip Specctra SES integration test.
//! Compiled only with the default-on `legacy` feature; `fr_core::legacy` explains why the
//! pre-rewrite pipeline is still here and when it goes.
#![cfg(feature = "legacy")]

use fr_core::legacy::RoutingJob;
use std::process::Command;

mod support;

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

#[test]
fn test_kicad_ses_import_roundtrip() {
    // Was `<repo>/upstream-freerouting`, a directory nothing creates — so this assertion below
    // was unconditionally red. See `support::upstream_tree`.
    let Some(tree) = support::upstream_tree() else {
        return;
    };
    let test_boards = [
        (
            "fixtures/Issue069-TestSensel/TestSensel.dsn",
            "fixtures/Issue069-TestSensel/TestSensel.kicad_pcb",
        ),
        (
            "examples/tutorial_board/tutorial_board.dsn",
            "examples/tutorial_board/tutorial_board.kicad_pcb",
        ),
    ];

    let kicad_python = find_kicad_python();

    for (rel_dsn, rel_pcb) in test_boards {
        let dsn_path = tree.join(rel_dsn);
        let pcb_path = tree.join(rel_pcb);

        if !dsn_path.exists() {
            eprintln!("SKIP {}: not in this corpus", dsn_path.display());
            continue;
        }
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
