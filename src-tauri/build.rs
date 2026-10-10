use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../frontend/src");
    println!("cargo:rerun-if-changed=../frontend/index.html");
    println!("cargo:rerun-if-changed=../frontend/package.json");
    println!("cargo:rerun-if-changed=../frontend/vite.config.js");

    let frontend_dir = Path::new("../frontend");
    let dist_index = frontend_dir.join("dist").join("index.html");
    let node_modules = frontend_dir.join("node_modules");

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };

    // Ensure dependencies are installed if missing
    if !node_modules.exists() {
        println!("cargo:warning=node_modules missing in frontend, running npm install...");
        let install_status = Command::new(npm)
            .arg("install")
            .current_dir(frontend_dir)
            .status();

        if let Err(e) = install_status {
            println!("cargo:warning=Failed to invoke npm install: {}", e);
        }
    }

    // Always compile frontend for production or if dist is missing
    let build_status = Command::new(npm)
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status();

    match build_status {
        Ok(status) if status.success() => {
            // Frontend built successfully
        }
        Ok(status) => {
            if dist_index.exists() {
                println!(
                    "cargo:warning=npm run build exited with {}, using existing dist assets",
                    status
                );
            } else {
                panic!(
                    "npm run build failed with {} and no pre-built frontend/dist was found.",
                    status
                );
            }
        }
        Err(e) => {
            if dist_index.exists() {
                println!(
                    "cargo:warning=Failed to execute npm ({}), using existing dist assets",
                    e
                );
            } else {
                panic!(
                    "Failed to execute npm ({}) and no pre-built frontend/dist was found. Ensure Node.js and npm are installed.",
                    e
                );
            }
        }
    }

    tauri_build::build();
}
