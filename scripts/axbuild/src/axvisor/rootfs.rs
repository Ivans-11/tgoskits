//! Axvisor-specific rootfs resolution and preparation helpers.
//!
//! Main responsibilities:
//! - Resolve which rootfs image Axvisor should use for a QEMU run
//! - Distinguish between explicit, managed, and VM-config-derived rootfs paths
//! - Prepare managed rootfs images before launch when Axvisor relies on them
//! - Patch QEMU configs with the selected rootfs using Axvisor-specific rules

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{
    Axvisor, build,
    image::{
        config::{ImageConfig, fallback_registry_url},
        registry::ImageRegistry,
        spec::ImageSpecRef,
        storage::Storage,
    },
};
use crate::{context::ResolvedAxvisorRequest, rootfs, support::download::http_client};

const AXVISOR_LINUX_KERNEL_PATH: &str = "/guest/linux/linux-qemu";
const AXVISOR_LINUX_ROOTFS_HEADROOM: u64 = 16 * 1024 * 1024;
const AXVISOR_LINUX_ROOTFS_FORMAT: u32 = 3;

pub(super) async fn qemu(axvisor: &mut Axvisor, args: super::ArgsQemu) -> anyhow::Result<()> {
    let request = axvisor.prepare_request(
        (&args.build).into(),
        args.qemu_config,
        None,
        crate::context::SnapshotPersistence::Store,
    )?;
    axvisor.app.set_debug_mode(request.debug)?;
    let cargo = build::load_cargo_config(&request)?;
    let explicit_rootfs = args.rootfs.map(|rootfs| {
        crate::rootfs::store::resolve_explicit_rootfs(
            axvisor.app.workspace_root(),
            &request.arch,
            rootfs,
        )
    });
    ensure_qemu_rootfs_ready(
        &request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )
    .await?;
    let qemu =
        load_patched_qemu_config(axvisor, &request, &cargo, explicit_rootfs.as_deref()).await?;
    axvisor
        .app
        .qemu(cargo, request.build_info_path, Some(qemu))
        .await
}

pub(super) async fn load_patched_qemu_config(
    axvisor: &mut Axvisor,
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<QemuConfig> {
    let config_path = request.qemu_config.clone().unwrap_or_else(|| {
        super::default_qemu_config_template_path(&request.axvisor_dir, &request.arch)
    });
    let mut qemu = axvisor
        .app
        .tool_mut()
        .read_qemu_config_from_path_for_cargo(cargo, &config_path)
        .await?;
    patch_qemu_rootfs(
        &mut qemu,
        request,
        axvisor.app.workspace_root(),
        explicit_rootfs,
    )?;
    Ok(qemu)
}

/// Ensures the managed rootfs required by an Axvisor QEMU run is available.
pub(crate) async fn ensure_qemu_rootfs_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    if explicit_rootfs.is_none()
        && infer_rootfs_path(&request.vmconfigs)?.is_none()
        && axvisor_linux_image_name(&request.arch).is_some()
    {
        ensure_axvisor_linux_rootfs(workspace_root, &request.arch).await?;
        return Ok(());
    }

    let rootfs_path = managed_rootfs_path(request, workspace_root, explicit_rootfs)?;
    rootfs::store::ensure_optional_managed_rootfs(
        workspace_root,
        &request.arch,
        rootfs_path.as_deref(),
    )
    .await
}

/// Patches a QEMU config with the rootfs selected for an Axvisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    patch_qemu_rootfs_path(config, &rootfs_path);
    Ok(())
}

/// Resolves the rootfs path selected for an Axvisor QEMU request.
pub(crate) fn qemu_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit_rootfs {
        return Ok(explicit.to_path_buf());
    }

    infer_rootfs_path(&request.vmconfigs)?
        .map(Ok)
        .unwrap_or_else(|| {
            axvisor_linux_rootfs_path(workspace_root, &request.arch)
                .map(Ok)
                .unwrap_or_else(|| {
                    rootfs::store::default_rootfs_path(workspace_root, &request.arch)
                })
        })
}

/// Patches a QEMU config with a concrete Axvisor rootfs path.
pub(crate) fn patch_qemu_rootfs_path(config: &mut QemuConfig, rootfs_path: &Path) {
    rootfs::qemu::patch_rootfs(
        config,
        rootfs_path,
        rootfs::qemu::RootfsPatchMode::ReplaceDriveOnly,
    );
}

/// Returns the managed rootfs path Axvisor should prepare, if any.
pub(crate) fn managed_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit_rootfs) = explicit_rootfs {
        if explicit_rootfs.starts_with(rootfs::store::rootfs_dir(workspace_root)) {
            return Ok(Some(explicit_rootfs.to_path_buf()));
        }
        return Ok(None);
    }

    if infer_rootfs_path(&request.vmconfigs)?.is_none() {
        if axvisor_linux_rootfs_path(workspace_root, &request.arch).is_some() {
            return Ok(None);
        }
        return Ok(Some(rootfs::store::default_rootfs_path(
            workspace_root,
            &request.arch,
        )?));
    }

    Ok(None)
}

fn axvisor_linux_image_name(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("qemu_aarch64_linux"),
        "riscv64" => Some("qemu_riscv64_linux"),
        "x86_64" => Some("qemu_x86_64_linux"),
        _ => None,
    }
}

fn axvisor_linux_kernel_name(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("qemu-aarch64"),
        "riscv64" => Some("qemu-riscv64"),
        "x86_64" => Some("qemu-x86_64"),
        _ => None,
    }
}

fn axvisor_linux_rootfs_path(workspace_root: &Path, arch: &str) -> Option<PathBuf> {
    axvisor_linux_image_name(arch).map(|_| {
        rootfs::store::rootfs_dir(workspace_root).join(format!("axvisor-{arch}-linux.img"))
    })
}

async fn ensure_axvisor_linux_rootfs(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let image_name = axvisor_linux_image_name(arch)
        .ok_or_else(|| anyhow!("no packaged Axvisor Linux image for architecture `{arch}`"))?;
    let kernel_name = axvisor_linux_kernel_name(arch)
        .ok_or_else(|| anyhow!("no packaged Axvisor Linux kernel for architecture `{arch}`"))?;

    let config = ImageConfig::read_config(workspace_root)?;
    let storage = Storage::new_from_config(&config).await?;
    let spec = ImageSpecRef::parse(image_name);
    let image_dir = match storage.pull_image(spec, None, true).await {
        Ok(image_dir) => image_dir,
        Err(primary_err) if primary_err.to_string().starts_with("image not found:") => {
            let client = http_client()?;
            let fallback_url = fallback_registry_url();
            let fallback_registry = ImageRegistry::fetch_with_includes(&client, &fallback_url)
                .await
                .with_context(|| {
                    format!("failed to fetch Axvisor fallback image registry {fallback_url}")
                })?;
            Storage {
                path: config.local_storage.clone(),
                image_registry: fallback_registry,
            }
            .pull_image(spec, None, true)
            .await
            .with_context(|| {
                format!(
                    "failed to prepare Axvisor guest image `{image_name}` after the current \
                     registry reported: {primary_err}"
                )
            })?
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to prepare Axvisor guest image `{image_name}`"));
        }
    };
    let source_rootfs = image_dir.join("rootfs.img");
    let source_kernel = image_dir.join(kernel_name);
    if !source_rootfs.is_file() || !source_kernel.is_file() {
        bail!(
            "Axvisor guest image `{image_name}` is incomplete: expected {} and {}",
            source_rootfs.display(),
            source_kernel.display()
        );
    }

    let archive_hash = fs::read_to_string(image_dir.join(".archive.sha256"))
        .with_context(|| format!("failed to identify Axvisor guest image `{image_name}`"))?;
    let rootfs_path = axvisor_linux_rootfs_path(workspace_root, arch)
        .expect("supported Axvisor Linux architecture must have a rootfs path");
    let marker_path = rootfs_path.with_extension("img.source");
    let source_key = format!(
        "{image_name}:{}:format-{AXVISOR_LINUX_ROOTFS_FORMAT}",
        archive_hash.trim()
    );
    if rootfs_path.is_file()
        && fs::read_to_string(&marker_path)
            .map(|marker| marker.trim() == source_key)
            .unwrap_or(false)
    {
        return Ok(rootfs_path);
    }

    let rootfs_dir = rootfs_path
        .parent()
        .expect("Axvisor Linux rootfs path must have a parent");
    fs::create_dir_all(rootfs_dir)
        .with_context(|| format!("failed to create {}", rootfs_dir.display()))?;
    let temporary = rootfs_path.with_extension("img.part");
    if temporary.exists() {
        fs::remove_file(&temporary)
            .with_context(|| format!("failed to remove {}", temporary.display()))?;
    }
    fs::copy(&source_rootfs, &temporary).with_context(|| {
        format!(
            "failed to copy Axvisor guest rootfs {} to {}",
            source_rootfs.display(),
            temporary.display()
        )
    })?;
    grow_rootfs_for_kernel(&temporary, &source_kernel)?;
    inject_axvisor_linux_kernel(&temporary, &source_kernel, rootfs_dir, arch)?;
    fs::rename(&temporary, &rootfs_path).with_context(|| {
        format!(
            "failed to install prepared Axvisor rootfs {}",
            rootfs_path.display()
        )
    })?;
    fs::write(&marker_path, format!("{source_key}\n"))
        .with_context(|| format!("failed to write {}", marker_path.display()))?;
    Ok(rootfs_path)
}

fn inject_axvisor_linux_kernel(
    rootfs_path: &Path,
    kernel_path: &Path,
    rootfs_dir: &Path,
    arch: &str,
) -> anyhow::Result<()> {
    let overlay_dir = rootfs_dir.join(format!(".axvisor-{arch}-linux-overlay"));
    if overlay_dir.exists() {
        fs::remove_dir_all(&overlay_dir)
            .with_context(|| format!("failed to remove {}", overlay_dir.display()))?;
    }
    let overlay_kernel = overlay_dir.join(AXVISOR_LINUX_KERNEL_PATH.trim_start_matches('/'));
    fs::create_dir_all(
        overlay_kernel
            .parent()
            .expect("Axvisor Linux kernel path must have a parent"),
    )
    .with_context(|| format!("failed to create Axvisor Linux kernel overlay for {arch}"))?;
    fs::copy(kernel_path, &overlay_kernel)
        .with_context(|| format!("failed to stage {}", kernel_path.display()))?;

    let result = rootfs::inject::inject_overlay(rootfs_path, &overlay_dir);
    let cleanup = fs::remove_dir_all(&overlay_dir)
        .with_context(|| format!("failed to remove {}", overlay_dir.display()));
    result?;
    cleanup
}

fn grow_rootfs_for_kernel(rootfs_path: &Path, kernel_path: &Path) -> anyhow::Result<()> {
    let rootfs_size = fs::metadata(rootfs_path)
        .with_context(|| format!("failed to stat {}", rootfs_path.display()))?
        .len();
    let kernel_size = fs::metadata(kernel_path)
        .with_context(|| format!("failed to stat {}", kernel_path.display()))?
        .len();
    let required_size = rootfs_size
        .checked_add(kernel_size)
        .and_then(|size| size.checked_add(AXVISOR_LINUX_ROOTFS_HEADROOM))
        .ok_or_else(|| anyhow!("Axvisor Linux rootfs size overflow"))?;
    let expanded_size = required_size.next_multiple_of(1024 * 1024);

    fs::OpenOptions::new()
        .write(true)
        .open(rootfs_path)
        .with_context(|| format!("failed to open {}", rootfs_path.display()))?
        .set_len(expanded_size)
        .with_context(|| format!("failed to expand {}", rootfs_path.display()))?;

    let status = Command::new("resize2fs")
        .arg(rootfs_path)
        .status()
        .with_context(|| "failed to run resize2fs while preparing the Axvisor Linux rootfs")?;
    if !status.success() {
        bail!(
            "failed to resize Axvisor Linux rootfs {}: resize2fs exited with {status}",
            rootfs_path.display()
        );
    }
    Ok(())
}

/// Infers a rootfs image path from VM config files by looking next to the
/// configured guest kernel image.
pub(crate) fn infer_rootfs_path(vmconfigs: &[PathBuf]) -> anyhow::Result<Option<PathBuf>> {
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let Some(kernel_path) = value
            .get("kernel")
            .and_then(|kernel| kernel.get("kernel_path"))
            .and_then(|path| path.as_str())
        else {
            continue;
        };
        let rootfs_path = Path::new(kernel_path)
            .parent()
            .map(|dir| dir.join("rootfs.img"));
        if let Some(rootfs_path) = rootfs_path
            && rootfs_path.exists()
        {
            return Ok(Some(rootfs_path));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn request(root: &Path, vmconfigs: Vec<PathBuf>) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: root.join(".build.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs,
        }
    }

    #[test]
    fn infer_rootfs_path_uses_vmconfig_kernel_sibling() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            infer_rootfs_path(&[vmconfig]).unwrap(),
            Some(image_dir.join("rootfs.img"))
        );
    }

    #[test]
    fn patch_qemu_rootfs_overrides_rootfs_when_vmconfig_provides_one() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        let rootfs_path = image_dir.join("rootfs.img");
        fs::write(&rootfs_path, b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };
        patch_qemu_rootfs(
            &mut qemu,
            &request(root.path(), vec![vmconfig]),
            root.path(),
            None,
        )
        .unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                rootfs_path.display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_uses_packaged_linux_rootfs_by_default() {
        let root = tempdir().unwrap();
        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                root.path()
                    .join("tmp/axbuild/rootfs/axvisor-aarch64-linux.img")
                    .display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_inserts_drive_arg_when_template_omits_it() {
        let root = tempdir().unwrap();
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
            ],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("tmp/axbuild/rootfs/axvisor-aarch64-linux.img")
                        .display()
                ),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
            ]
        );
    }

    #[test]
    fn managed_rootfs_path_skips_generic_download_for_packaged_linux_rootfs() {
        let root = tempdir().unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "/tmp/qemu-aarch64"
"#,
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            None
        );
    }

    #[test]
    fn managed_rootfs_path_skips_when_vmconfig_provides_kernel_sibling_rootfs() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            None
        );
    }

    #[test]
    fn managed_rootfs_path_keeps_explicit_managed_rootfs() {
        let root = tempdir().unwrap();
        let explicit = root
            .path()
            .join("tmp/axbuild/rootfs/rootfs-aarch64-debian.img");

        assert_eq!(
            managed_rootfs_path(
                &request(root.path(), vec![]),
                root.path(),
                Some(explicit.as_path())
            )
            .unwrap(),
            Some(explicit)
        );
    }
}
