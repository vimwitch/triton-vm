fn main() {
    let git_dir = maybe_get_git_dir();
    trigger_rebuild_if_head_or_some_relevant_ref_changes(git_dir);

    let git_output = std::process::Command::new("git")
        .args(["describe", "--always", "--tags", "--long", "--dirty"])
        .output()
        .ok();
    let git_info = git_output
        .as_ref()
        .and_then(|output| std::str::from_utf8(&output.stdout).ok().map(str::trim));
    let cargo_pkg_version = env!("CARGO_PKG_VERSION");

    // Default git_describe to cargo_pkg_version
    let mut git_describe = String::from(cargo_pkg_version);

    if let Some(git_info) = git_info {
        // If the `git_info` contains `CARGO_PKG_VERSION`, we simply use `git_info` as it is.
        // Otherwise, prepend `CARGO_PKG_VERSION` to `git_info`.
        if git_info.contains(cargo_pkg_version) {
            // Remove the 'g' before the commit sha
            let git_info = &git_info.replace('g', "");
            git_describe = git_info.to_string();
        } else {
            git_describe = format!("v{cargo_pkg_version}-{git_info}");
        }
    }

    println!("cargo:rustc-env=TRITON_TUI_GIT_INFO={git_describe}");
}

fn maybe_get_git_dir() -> Option<String> {
    let git_output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok();
    git_output.as_ref().and_then(|output| {
        std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|s| s.strip_suffix('\n').or_else(|| s.strip_suffix("\r\n")))
            .map(str::to_string)
    })
}

fn trigger_rebuild_if_head_or_some_relevant_ref_changes(git_dir: Option<String>) {
    if let Some(git_dir) = git_dir {
        let git_path = std::path::Path::new(&git_dir);
        let refs_path = git_path.join("refs");
        if git_path.join("HEAD").exists() {
            println!("cargo:rerun-if-changed={git_dir}/HEAD");
        }
        if git_path.join("packed-refs").exists() {
            println!("cargo:rerun-if-changed={git_dir}/packed-refs");
        }
        if refs_path.join("heads").exists() {
            println!("cargo:rerun-if-changed={git_dir}/refs/heads");
        }
        if refs_path.join("tags").exists() {
            println!("cargo:rerun-if-changed={git_dir}/refs/tags");
        }
    }
}
