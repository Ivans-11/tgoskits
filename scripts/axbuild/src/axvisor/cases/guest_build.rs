use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use ostool::build::config::{Cargo, LogLevel};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    context::{workspace_manifest_path, workspace_metadata_root_manifest, workspace_root_path},
    process::ProcessExt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxFeaturePrefixFamily {
    AxStd,
    AxFeat,
}

impl AxFeaturePrefixFamily {
    fn prefix(self) -> &'static str {
        match self {
            Self::AxStd => "ax-std/",
            Self::AxFeat => "ax-feat/",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub(super) struct CaseBuildInfo {
    pub(super) env: HashMap<String, String>,
    pub(super) features: Vec<String>,
    pub(super) log: LogLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) max_cpu_num: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) plat_dyn: bool,
}

impl CaseBuildInfo {
    pub(super) fn default_for_target(target: &str) -> Self {
        Self {
            plat_dyn: supports_platform_dynamic(target),
            ..Self::default()
        }
    }

    fn effective_plat_dyn(&self, target: &str, plat_dyn_override: Option<bool>) -> bool {
        resolve_effective_plat_dyn(target, self.plat_dyn, plat_dyn_override)
    }

    fn normalize_legacy_feature_aliases(&mut self) -> bool {
        let mut changed = false;

        for feature in &mut self.features {
            let normalized = normalize_legacy_feature_alias(feature);
            if *feature != normalized {
                *feature = normalized;
                changed = true;
            }
        }

        if changed {
            self.features.sort();
            self.features.dedup();
        }

        changed
    }

    fn prepare_log_env(&mut self) {
        self.env
            .insert("AX_LOG".into(), format!("{:?}", self.log).to_lowercase());
    }

    fn prepare_max_cpu_num_env(&mut self) -> anyhow::Result<()> {
        if let Some(max_cpu_num) = self.validated_max_cpu_num()? {
            self.env.insert("SMP".into(), max_cpu_num.to_string());
        }
        Ok(())
    }

    fn resolve_features(&mut self, package: &str, plat_dyn: bool) {
        let prefix_family = self.resolve_ax_feature_prefix_family(package);
        let has_myplat = self.features.iter().any(|feature| {
            matches!(
                feature.as_str(),
                "myplat" | "ax-std/myplat" | "ax-feat/myplat"
            )
        });

        self.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "plat-dyn"
                    | "defplat"
                    | "myplat"
                    | "ax-std/plat-dyn"
                    | "ax-std/defplat"
                    | "ax-std/myplat"
                    | "ax-feat/plat-dyn"
                    | "ax-feat/defplat"
                    | "ax-feat/myplat"
            )
        });

        if plat_dyn {
            self.features
                .push(format!("{}plat-dyn", prefix_family.prefix()));
        } else if has_myplat {
            self.features
                .push(format!("{}myplat", prefix_family.prefix()));
        } else {
            self.features
                .push(format!("{}defplat", prefix_family.prefix()));
        }

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push(format!("{}smp", prefix_family.prefix()));
        }

        self.features.sort();
        self.features.dedup();
    }

    fn resolve_ax_feature_prefix_family(&self, package: &str) -> AxFeaturePrefixFamily {
        match detect_ax_feature_prefix_family(package) {
            Ok(prefix_family) => prefix_family,
            Err(err) => {
                if let Some(prefix_family) = feature_family_from_existing_features(&self.features) {
                    return prefix_family;
                }
                warn!(
                    "failed to detect direct ax dependency for package {}: {}, defaulting to \
                     ax-std feature prefix",
                    package, err
                );
                AxFeaturePrefixFamily::AxStd
            }
        }
    }

    fn prepare_non_dynamic_platform_for(
        &mut self,
        package: &str,
        target: &str,
        plat_dyn: bool,
    ) -> anyhow::Result<()> {
        if plat_dyn {
            return Ok(());
        }

        ensure_arceos_tooling_installed()?;

        let package_manifest = resolve_package_manifest_path(package)?;
        let app_dir = package_manifest
            .parent()
            .context("package manifest path has no parent directory")?;
        let platform_package = resolve_platform_package(package, target, &self.features)?;
        let platform_config = resolve_platform_config_path(app_dir, &platform_package)?;
        let platform_name = read_platform_name(&platform_config)
            .unwrap_or_else(|| linker_platform_name(&platform_package).to_string());
        let out_config = app_dir.join(".axconfig.toml");

        generate_axconfig(
            &workspace_root_path()?,
            target,
            &platform_name,
            &platform_config,
            &out_config,
            self.validated_max_cpu_num()?,
        )?;

        self.env.insert(
            "AX_CONFIG_PATH".to_string(),
            out_config.display().to_string(),
        );
        self.env
            .insert("AX_PLATFORM".to_string(), platform_name.to_string());

        Ok(())
    }

    fn validated_max_cpu_num(&self) -> anyhow::Result<Option<usize>> {
        match self.max_cpu_num {
            Some(0) => bail!("max_cpu_num must be greater than 0"),
            Some(max_cpu_num) => Ok(Some(max_cpu_num)),
            None => Ok(None),
        }
    }

    pub(super) fn into_prepared_cargo_config(
        mut self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
    ) -> anyhow::Result<Cargo> {
        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.validated_max_cpu_num()?;
        self.prepare_non_dynamic_platform_for(package, target, plat_dyn)?;
        self.resolve_features(package, plat_dyn);
        self.prepare_log_env();
        self.prepare_max_cpu_num_env()?;

        Ok(Cargo {
            env: self.env,
            target: target.to_string(),
            package: package.to_string(),
            features: self.features,
            log: Some(self.log),
            extra_config: None,
            args: build_cargo_args(target, plat_dyn),
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin: default_to_bin_for_target(target),
        })
    }
}

impl Default for CaseBuildInfo {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("AX_IP".to_string(), "10.0.2.15".to_string());
        env.insert("AX_GW".to_string(), "10.0.2.2".to_string());

        Self {
            env,
            log: LogLevel::Warn,
            features: vec!["ax-std".to_string()],
            max_cpu_num: None,
            plat_dyn: false,
        }
    }
}

pub(super) fn load_or_create_build_info<T>(
    path: &Path,
    default: impl FnOnce() -> T,
) -> anyhow::Result<T>
where
    T: Serialize + DeserializeOwned,
{
    println!("Using build config: {}", path.display());

    if path.exists() {
        info!("Found build config at {}", path.display());
    } else {
        info!(
            "Build config not found at {}, writing default config",
            path.display()
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, toml::to_string_pretty(&default())?)?;
    }

    toml::from_str::<T>(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse build info {}", path.display()))
}

pub(super) fn load_case_build_info(path: &Path, target: &str) -> anyhow::Result<CaseBuildInfo> {
    let mut build_info =
        load_or_create_build_info(path, || CaseBuildInfo::default_for_target(target))?;

    if build_info.normalize_legacy_feature_aliases() {
        warn!(
            "normalizing legacy feature aliases in build config {}",
            path.display()
        );
        fs::write(path, toml::to_string_pretty(&build_info)?).with_context(|| {
            format!("failed to rewrite normalized build info {}", path.display())
        })?;
    }

    Ok(build_info)
}

pub(super) fn resolve_build_info_path_in_dir(dir: &Path, target: &str) -> PathBuf {
    let bare_path = dir.join(format!("build-{target}.toml"));
    if bare_path.exists() {
        return bare_path;
    }

    let dotted_path = dir.join(format!(".build-{target}.toml"));
    if dotted_path.exists() {
        return dotted_path;
    }

    dotted_path
}

fn resolve_effective_plat_dyn(
    target: &str,
    configured_plat_dyn: bool,
    plat_dyn_override: Option<bool>,
) -> bool {
    plat_dyn_override.unwrap_or(configured_plat_dyn) && supports_platform_dynamic(target)
}

fn supports_platform_dynamic(target: &str) -> bool {
    target.starts_with("aarch64-")
}

fn default_to_bin_for_target(target: &str) -> bool {
    !target.starts_with("x86_64-")
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn build_cargo_args(target: &str, plat_dyn: bool) -> Vec<String> {
    let mut args = Vec::new();
    args.push("--config".to_string());
    args.push(if plat_dyn {
        format!("target.{target}.rustflags=[\"-Clink-arg=-Taxplat.x\"]")
    } else {
        format!(
            "target.{target}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
             -Clink-arg=-znostart-stop-gc\"]"
        )
    });
    args
}

fn normalize_legacy_feature_alias(feature: &str) -> String {
    if feature == "axstd" {
        "ax-std".to_string()
    } else if let Some(rest) = feature.strip_prefix("axstd/") {
        format!("ax-std/{rest}")
    } else if feature == "axfeat" {
        "ax-feat".to_string()
    } else if let Some(rest) = feature.strip_prefix("axfeat/") {
        format!("ax-feat/{rest}")
    } else {
        feature.to_string()
    }
}

fn feature_family_from_existing_features(features: &[String]) -> Option<AxFeaturePrefixFamily> {
    if features
        .iter()
        .any(|feature| feature.starts_with("ax-std/"))
    {
        return Some(AxFeaturePrefixFamily::AxStd);
    }
    if features
        .iter()
        .any(|feature| feature.starts_with("ax-feat/"))
    {
        return Some(AxFeaturePrefixFamily::AxFeat);
    }
    None
}

fn detect_ax_feature_prefix_family(package: &str) -> anyhow::Result<AxFeaturePrefixFamily> {
    let manifest_path = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&manifest_path)?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;

    let has_axstd = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "ax-std" || dep.rename.as_deref() == Some("ax-std"));
    let has_axfeat = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "ax-feat" || dep.rename.as_deref() == Some("ax-feat"));

    match (has_axstd, has_axfeat) {
        (true, true) | (true, false) => Ok(AxFeaturePrefixFamily::AxStd),
        (false, true) => Ok(AxFeaturePrefixFamily::AxFeat),
        (false, false) => Err(anyhow!(
            "package `{package}` must directly depend on `ax-std` or `ax-feat`"
        )),
    }
}

fn resolve_package_manifest_path(package: &str) -> anyhow::Result<PathBuf> {
    let manifest_path = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&manifest_path)?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}

fn resolve_platform_package(
    package: &str,
    target: &str,
    features: &[String],
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    let workspace_manifest = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&workspace_manifest)?;
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;

    let explicit_platform_features: Vec<_> = features
        .iter()
        .map(|feature| {
            feature
                .strip_prefix("ax-feat/")
                .or_else(|| feature.strip_prefix("ax-std/"))
                .unwrap_or(feature.as_str())
        })
        .filter(|feature| {
            !matches!(
                *feature,
                "ax-std" | "ax-feat" | "plat-dyn" | "defplat" | "myplat"
            )
        })
        .collect();

    if let Some(dep) = package_info.dependencies.iter().find(|dep| {
        dependency_platform_config_path(&metadata, &dep.name)
            .ok()
            .flatten()
            .is_some_and(|config_path| {
                explicit_platform_features.iter().any(|feature| {
                    *feature == linker_platform_name(&dep.name)
                        || platform_config_matches_name(&config_path, feature)
                })
            })
    }) {
        return Ok(dep.name.clone());
    }

    if features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-std/myplat" | "ax-feat/myplat"
        )
    }) && let Some(dep) = package_info.dependencies.iter().find(|dep| {
        dependency_platform_config_path(&metadata, &dep.name)
            .ok()
            .flatten()
            .is_some_and(|config_path| platform_config_matches_arch(&config_path, arch))
    }) {
        return Ok(dep.name.clone());
    }

    Ok(default_platform_package(arch).to_string())
}

fn target_arch_name(target: &str) -> anyhow::Result<&'static str> {
    if target.starts_with("aarch64-") {
        Ok("aarch64")
    } else if target.starts_with("x86_64-") {
        Ok("x86_64")
    } else if target.starts_with("riscv64") {
        Ok("riscv64")
    } else if target.starts_with("loongarch64-") {
        Ok("loongarch64")
    } else {
        Err(anyhow!("unsupported target triple `{target}`"))
    }
}

fn default_platform_package(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "ax-plat-x86-pc",
        "aarch64" => "ax-plat-aarch64-qemu-virt",
        "riscv64" => "ax-plat-riscv64-qemu-virt",
        "loongarch64" => "ax-plat-loongarch64-qemu-virt",
        _ => unreachable!("unsupported arch"),
    }
}

fn linker_platform_name(platform_package: &str) -> &str {
    platform_package
        .strip_prefix("axplat-")
        .or_else(|| platform_package.strip_prefix("ax-plat-"))
        .unwrap_or(platform_package)
}

fn resolve_platform_config_path(app_dir: &Path, platform_package: &str) -> anyhow::Result<PathBuf> {
    if let Some(local_path) = find_local_platform_config_path(platform_package)? {
        return Ok(local_path);
    }

    let workspace_root = workspace_root_path()?;
    let root_manifest = workspace_root.join("Cargo.toml");
    let output = Command::new("cargo")
        .arg("axplat")
        .arg("info")
        .arg("--manifest-path")
        .arg(&root_manifest)
        .arg("-C")
        .arg(app_dir)
        .arg("-c")
        .arg(platform_package)
        .exec_capture()
        .with_context(|| format!("failed to run cargo axplat info for `{platform_package}`"))?;

    let config_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if config_path.is_empty() {
        bail!(
            "cargo axplat info returned empty config path for package `{}`",
            platform_package
        );
    }

    let config_path = PathBuf::from(config_path);
    if !config_path.exists() {
        bail!(
            "platform config path does not exist: {}",
            config_path.display()
        );
    }

    Ok(config_path)
}

fn find_local_platform_config_path(platform_package: &str) -> anyhow::Result<Option<PathBuf>> {
    let workspace_root = workspace_root_path()?;
    let platform_dir_name = platform_package
        .strip_prefix("ax-plat-")
        .map(|suffix| format!("axplat-{suffix}"))
        .unwrap_or_else(|| platform_package.to_string());
    let candidate = workspace_root
        .join("components/axplat_crates/platforms")
        .join(platform_dir_name)
        .join("axconfig.toml");

    Ok(candidate.exists().then_some(candidate))
}

fn dependency_platform_config_path(
    metadata: &cargo_metadata::Metadata,
    dependency_name: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = find_local_platform_config_path(dependency_name)? {
        return Ok(Some(path));
    }

    Ok(metadata
        .packages
        .iter()
        .find(|pkg| pkg.name == dependency_name)
        .and_then(|pkg| {
            let candidate = pkg
                .manifest_path
                .clone()
                .into_std_path_buf()
                .parent()
                .map(|parent| parent.join("axconfig.toml"))?;
            candidate.exists().then_some(candidate)
        }))
}

fn ensure_arceos_tooling_installed() -> anyhow::Result<()> {
    ensure_cargo_axplat_installed()?;
    ensure_ax_config_gen_installed()?;
    Ok(())
}

fn ensure_cargo_axplat_installed() -> anyhow::Result<()> {
    if Command::new("cargo")
        .arg("axplat")
        .arg("--version")
        .exec_capture()
        .is_ok()
    {
        return Ok(());
    }

    warn!("`cargo axplat` not found, installing `cargo-axplat` via cargo");
    Command::new("cargo")
        .arg("install")
        .arg("cargo-axplat")
        .exec()
        .context("failed to install cargo-axplat")?;
    Ok(())
}

fn ensure_ax_config_gen_installed() -> anyhow::Result<()> {
    if Command::new("ax-config-gen")
        .arg("--version")
        .exec_capture()
        .is_ok()
    {
        return Ok(());
    }

    let workspace_root = workspace_root_path()?;
    let ax_config_gen_dir = workspace_root.join("components/axconfig-gen/axconfig-gen");

    warn!(
        "`ax-config-gen` not found, installing from local path {}",
        ax_config_gen_dir.display()
    );
    Command::new("cargo")
        .arg("install")
        .arg("--path")
        .arg(&ax_config_gen_dir)
        .exec()
        .with_context(|| {
            format!(
                "failed to install ax-config-gen from {}",
                ax_config_gen_dir.display()
            )
        })?;
    Ok(())
}

fn read_platform_name(platform_config: &Path) -> Option<String> {
    let contents = fs::read_to_string(platform_config).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("platform")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn read_platform_arch(platform_config: &Path) -> Option<String> {
    let contents = fs::read_to_string(platform_config).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("arch")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn platform_config_matches_name(platform_config: &Path, expected: &str) -> bool {
    read_platform_name(platform_config).as_deref() == Some(expected)
}

fn platform_config_matches_arch(platform_config: &Path, arch: &str) -> bool {
    read_platform_arch(platform_config).as_deref() == Some(arch)
}

fn generate_axconfig(
    workspace_root: &Path,
    target: &str,
    platform_name: &str,
    platform_config: &Path,
    out_config: &Path,
    max_cpu_num: Option<usize>,
) -> anyhow::Result<()> {
    let defconfig = resolve_defconfig_path(workspace_root)?;
    let arch = target_arch_name(target)?;
    let mut command = Command::new("ax-config-gen");
    command
        .arg(defconfig)
        .arg(platform_config)
        .arg("-w")
        .arg(format!("arch=\"{arch}\""))
        .arg("-w")
        .arg(format!("platform=\"{platform_name}\""));
    if let Some(max_cpu_num) = max_cpu_num {
        command
            .arg("-w")
            .arg(format!("plat.max-cpu-num={max_cpu_num}"));
    }
    command
        .arg("-o")
        .arg(out_config)
        .exec()
        .context("failed to run ax-config-gen")?;

    Ok(())
}

fn resolve_defconfig_path(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let defconfig = workspace_root.join("os/arceos/configs/defconfig.toml");
    if defconfig.exists() {
        Ok(defconfig)
    } else {
        bail!("missing ArceOS defconfig: {}", defconfig.display())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn resolve_build_info_path_in_dir_prefers_existing_bare_name() {
        let root = tempdir().unwrap();
        let bare_path = root
            .path()
            .join("build-aarch64-unknown-none-softfloat.toml");
        fs::write(&bare_path, "features = []\nlog = \"Warn\"\n[env]\n").unwrap();

        let path = resolve_build_info_path_in_dir(root.path(), "aarch64-unknown-none-softfloat");
        assert_eq!(path, bare_path);
    }

    #[test]
    fn resolve_build_info_path_in_dir_falls_back_to_dotted_default() {
        let root = tempdir().unwrap();
        let path = resolve_build_info_path_in_dir(root.path(), "aarch64-unknown-none-softfloat");
        assert_eq!(
            path,
            root.path()
                .join(".build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn default_case_build_info_enables_dynamic_platform_only_for_aarch64() {
        assert!(CaseBuildInfo::default_for_target("aarch64-unknown-none-softfloat").plat_dyn);
        assert!(!CaseBuildInfo::default_for_target("riscv64gc-unknown-none-elf").plat_dyn);
    }
}
