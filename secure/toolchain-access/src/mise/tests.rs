use super::*;

fn cargo_install(install_path: PathBuf) -> io::Result<Install> {
    std::fs::create_dir(&install_path)?;
    let cargo = install_path.join("cargo");
    std::fs::write(&cargo, "")?;
    std::fs::set_permissions(cargo, std::fs::Permissions::from_mode(0o755))?;
    Ok(Install {
        version: "1.2.3".into(),
        install_path,
        installed: true,
    })
}

#[test]
fn requires_matching_installed_version_and_executable_inside_install() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let install = cargo_install(root.join("install"))?;
    let cargo = install.install_path.join("cargo");
    assert!(install.matches("1.2.3", "cargo"));
    assert!(!install.matches("1.2.4", "cargo"));
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o644))?;
    assert!(!install.matches("1.2.3", "cargo"));
    let system = cargo_install(root.join("system"))?;
    std::fs::remove_file(&cargo)?;
    std::os::unix::fs::symlink(system.install_path.join("cargo"), cargo)?;
    assert!(!install.matches("1.2.3", "cargo"));
    Ok(())
}

#[test]
fn linked_installation_is_not_eligible() -> io::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    let mut install = cargo_install(root.join("system"))?;
    let install_path = root.join("install");
    std::os::unix::fs::symlink(&install.install_path, &install_path)?;
    install.install_path = install_path;
    assert!(!install.matches("1.2.3", "cargo"));
    Ok(())
}
