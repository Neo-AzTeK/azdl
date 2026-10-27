use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../frontend/src");
    println!("cargo:rerun-if-changed=../frontend/index.html");
    println!("cargo:rerun-if-changed=../frontend/package.json");
    println!("cargo:rerun-if-changed=../frontend/package-lock.json");
    println!("cargo:rerun-if-changed=../frontend/vite.config.js");

    let frontend_dir = Path::new("../frontend");
    let node_modules = frontend_dir.join("node_modules");

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };

    // Ensure dependencies are installed if missing
    if !node_modules.exists() {
        println!("cargo:warning=node_modules missing in frontend, running npm install...");
        let install_status = Command::new(npm)
            .args(["install", "--legacy-peer-deps"])
            .current_dir(frontend_dir)
            .status();

        let install_status = install_status.expect("Failed to invoke npm install for the frontend");
        assert!(
            install_status.success(),
            "Frontend dependency installation failed: {install_status}"
        );
    }

    // Embed only assets built from the current frontend sources.
    let build_status = Command::new(npm)
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status();

    let build_status =
        build_status.expect("Failed to invoke npm run build. Install Node.js and npm.");
    assert!(
        build_status.success(),
        "Frontend build failed: {build_status}"
    );

    tauri_build::build();
}
