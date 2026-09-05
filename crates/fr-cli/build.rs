use std::process::Command;

fn get_git_hash() -> String {
    // 1. Explicit override or CI environment variable
    if let Ok(hash) =
        std::env::var("FAST_ROUTING_GIT_HASH").or_else(|_| std::env::var("GITHUB_SHA"))
    {
        let trimmed = hash.trim();
        if !trimmed.is_empty() {
            let short = if trimmed.len() > 8 {
                &trimmed[..8]
            } else {
                trimmed
            };
            return short.to_string();
        }
    }

    // 2. Git CLI query
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
    {
        if output.status.success() {
            let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !hash.is_empty() {
                return hash;
            }
        }
    }

    // 3. Fallback when built outside git repository / offline tarball
    "unknown".to_string()
}

fn main() {
    let git_hash = get_git_hash();
    println!("cargo:rustc-env=FR_BUILD_GIT_HASH={git_hash}");
    println!("cargo:rustc-env=FR_REFERENCE_JAR_VERSION=2.3.0");
    println!("cargo:rustc-env=FR_REFERENCE_JAR_REVISION=2d4de019");

    println!("cargo:rerun-if-env-changed=FAST_ROUTING_GIT_HASH");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
}
