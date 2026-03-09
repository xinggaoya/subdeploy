use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgAction, Args, Parser, Subcommand};
use subdeploy_core::{default_remote_dir, deploy, ComposeSpec, DeployRequest, HealthCheckSpec};
use subdeploy_packager::{
    inspect_project, list_compose_services, package_project, PackageRequest, ProjectInspection,
};
use subdeploy_remote::SshRemote;

#[derive(Debug, Parser)]
#[command(
    name = "sd",
    version,
    about = "通过 SSH 部署 Docker 项目到远端服务器",
    disable_help_flag = true,
    after_help = "省略 deploy 子命令时，默认执行部署。例如：sd -u root -p secret -h example.com"
)]
struct Cli {
    /// 打印帮助
    #[arg(long = "help", action = ArgAction::Help, global = true)]
    _help: Option<bool>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// 校验项目是否满足部署前提
    Validate(ProjectArgs),
    /// 生成部署归档
    Package(PackageArgs),
    /// 执行完整部署
    Deploy(DeployArgs),
}

#[derive(Debug, Clone, Args)]
struct ProjectArgs {
    /// 项目根目录
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Dockerfile 相对路径或绝对路径
    #[arg(long)]
    dockerfile: Option<PathBuf>,
    /// compose 文件相对路径或绝对路径
    #[arg(long)]
    compose_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PackageArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// 输出归档路径
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct DeployArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// 远端主机
    #[arg(short = 'h', long)]
    host: String,
    /// SSH 端口
    #[arg(short = 'P', long, default_value_t = 22)]
    port: u16,
    /// SSH 用户名
    #[arg(short = 'u', long)]
    user: String,
    /// SSH 密码
    #[arg(short = 'p', long)]
    password: String,
    /// 容器服务名；不传时会尝试从 compose 文件自动推断
    #[arg(long)]
    service: Option<String>,
    /// 远端构建的镜像标签，默认 <project_name>:latest
    #[arg(long)]
    image_tag: Option<String>,
    /// 远端部署目录，默认 /root/<project>-deploy
    #[arg(long)]
    remote_dir: Option<String>,
    /// 健康检查地址；不传则只依赖远端命令执行成功
    #[arg(long)]
    health_url: Option<String>,
    /// 健康检查超时秒数
    #[arg(long, default_value_t = 300)]
    health_timeout_secs: u64,
    /// 健康检查轮询间隔秒数
    #[arg(long, default_value_t = 5)]
    poll_interval_secs: u64,
    /// 远端 docker build 使用 --no-cache
    #[arg(long, default_value_t = false)]
    no_cache: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("错误: {error:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse_from(normalize_args(env::args_os()));
    match cli.command {
        Commands::Validate(args) => run_validate(args),
        Commands::Package(args) => run_package(args),
        Commands::Deploy(args) => run_deploy(args),
    }
}

fn run_validate(args: ProjectArgs) -> Result<()> {
    let inspection = inspect_project(&PackageRequest::from(args))?;

    println!("项目校验通过");
    println!("项目目录: {}", inspection.project_dir.display());
    println!("项目名: {}", inspection.project_name);
    println!("Dockerfile: {}", inspection.dockerfile_rel.display());
    println!("Compose 文件: {}", inspection.compose_file_rel.display());
    println!("归档文件数: {}", inspection.included_files.len());
    println!(
        "默认远端目录: {}",
        default_remote_dir(&inspection.project_name)
    );

    Ok(())
}

fn run_package(args: PackageArgs) -> Result<()> {
    let request = PackageRequest::from(args.project);
    let output = args.output;
    let package = package_project(&request, &output)?;

    println!("归档生成完成");
    println!("输出文件: {}", package.archive_path.display());
    println!("归档文件数: {}", package.included_files.len());
    println!("Dockerfile: {}", package.dockerfile_rel.display());
    println!("Compose 文件: {}", package.compose_file_rel.display());

    Ok(())
}

fn run_deploy(args: DeployArgs) -> Result<()> {
    let request = PackageRequest::from(args.project.clone());
    let inspection = inspect_project(&request)?;
    let compose = build_compose_spec(&args, &inspection)?;
    let remote_dir = args
        .remote_dir
        .unwrap_or_else(|| default_remote_dir(&inspection.project_name));

    let health_check = args.health_url.map(|url| HealthCheckSpec {
        url,
        timeout: Duration::from_secs(args.health_timeout_secs),
        poll_interval: Duration::from_secs(args.poll_interval_secs),
    });

    let deploy_request = DeployRequest {
        package_request: request,
        remote_dir,
        ssh_host: args.host,
        ssh_port: args.port,
        ssh_user: args.user,
        ssh_password: args.password,
        compose,
        no_cache: args.no_cache,
        health_check: health_check.clone(),
    };

    println!(
        "开始部署: {}@{}:{}",
        deploy_request.ssh_user, deploy_request.ssh_host, deploy_request.ssh_port
    );
    println!("项目目录: {}", inspection.project_dir.display());
    println!("远端目录: {}", deploy_request.remote_dir);
    println!(
        "Compose 文件: {}",
        deploy_request.compose.compose_file_rel.display()
    );
    println!("服务名: {}", deploy_request.compose.service_name);
    println!("镜像标签: {}", deploy_request.compose.image_tag);

    let mut remote = SshRemote::connect(
        &deploy_request.ssh_host,
        deploy_request.ssh_port,
        &deploy_request.ssh_user,
        &deploy_request.ssh_password,
    )
    .with_context(|| "建立 SSH 连接失败")?;

    let result = deploy(&deploy_request, &mut remote)?;
    println!("远端归档: {}", result.remote_archive_path);
    println!("Release ID: {}", result.release_id);

    println!("部署完成");
    Ok(())
}

fn normalize_args<I, T>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut normalized: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let first_arg = normalized.get(1).and_then(|arg| arg.to_str());
    let has_explicit_command = matches!(
        first_arg,
        Some("validate" | "package" | "deploy" | "help" | "--help" | "--version" | "-V")
    );

    if !has_explicit_command {
        normalized.insert(1, OsString::from("deploy"));
    }

    normalized
}

fn build_compose_spec(args: &DeployArgs, inspection: &ProjectInspection) -> Result<ComposeSpec> {
    Ok(ComposeSpec {
        dockerfile_rel: inspection.dockerfile_rel.clone(),
        compose_file_rel: inspection.compose_file_rel.clone(),
        service_name: resolve_service_name(args, inspection)?,
        image_tag: args
            .image_tag
            .clone()
            .unwrap_or_else(|| default_image_tag(&inspection.project_name)),
    })
}

fn resolve_service_name(args: &DeployArgs, inspection: &ProjectInspection) -> Result<String> {
    if let Some(service) = &args.service {
        return Ok(service.clone());
    }

    let services = list_compose_services(&inspection.project_dir, &inspection.compose_file_rel)
        .map_err(|error| {
            anyhow!(
                "无法从 Compose 文件 {} 自动推断服务名: {}。请使用 --service 显式指定",
                inspection.compose_file_rel.display(),
                error
            )
        })?;

    match services.as_slice() {
        [service_name] => Ok(service_name.clone()),
        [] => bail!(
            "Compose 文件 {} 未找到服务定义，请使用 --service 显式指定",
            inspection.compose_file_rel.display()
        ),
        _ => bail!(
            "Compose 文件 {} 包含多个服务: {}。请使用 --service 显式指定",
            inspection.compose_file_rel.display(),
            services.join(", ")
        ),
    }
}

fn default_image_tag(project_name: &str) -> String {
    format!("{project_name}:latest")
}

impl From<ProjectArgs> for PackageRequest {
    fn from(value: ProjectArgs) -> Self {
        Self {
            project_dir: value.project_dir,
            dockerfile: value.dockerfile,
            compose_file: value.compose_file,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::error::ErrorKind;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn normalize_args_inserts_deploy_by_default() {
        let args = normalize_args(["sd", "-u", "root", "-p", "secret", "-h", "example.com"]);
        assert_eq!(
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>(),
            vec![
                "sd",
                "deploy",
                "-u",
                "root",
                "-p",
                "secret",
                "-h",
                "example.com"
            ]
        );
    }

    #[test]
    fn normalize_args_keeps_explicit_subcommand() {
        let args = normalize_args(["sd", "validate", "--project-dir", "."]);
        assert_eq!(
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>(),
            vec!["sd", "validate", "--project-dir", "."]
        );
    }

    #[test]
    fn cli_parses_short_deploy_flags() {
        let cli = Cli::try_parse_from(normalize_args([
            "sd",
            "-u",
            "root",
            "-p",
            "secret",
            "-h",
            "example.com",
        ]))
        .unwrap();

        match cli.command {
            Commands::Deploy(args) => {
                assert_eq!(args.user, "root");
                assert_eq!(args.password, "secret");
                assert_eq!(args.host, "example.com");
                assert_eq!(args.port, 22);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_keeps_long_help_flag() {
        let error = Cli::try_parse_from(normalize_args(["sd", "--help"])).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    }

    #[test]
    fn build_compose_spec_uses_defaults_for_single_service() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: demo:latest\n",
        )
        .unwrap();

        let inspection = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap();
        let args = DeployArgs {
            project: ProjectArgs {
                project_dir: dir.path().to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            host: "example.com".to_owned(),
            port: 22,
            user: "root".to_owned(),
            password: "secret".to_owned(),
            service: None,
            image_tag: None,
            remote_dir: None,
            health_url: None,
            health_timeout_secs: 300,
            poll_interval_secs: 5,
            no_cache: false,
        };

        let compose = build_compose_spec(&args, &inspection).unwrap();
        assert_eq!(compose.service_name, "web");
        assert_eq!(
            compose.image_tag,
            format!("{}:latest", inspection.project_name)
        );
    }

    #[test]
    fn build_compose_spec_requires_explicit_service_for_multi_service_compose() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  api:\n    image: demo:latest\n  worker:\n    image: demo:latest\n",
        )
        .unwrap();

        let inspection = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap();
        let args = DeployArgs {
            project: ProjectArgs {
                project_dir: dir.path().to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            host: "example.com".to_owned(),
            port: 22,
            user: "root".to_owned(),
            password: "secret".to_owned(),
            service: None,
            image_tag: None,
            remote_dir: None,
            health_url: None,
            health_timeout_secs: 300,
            poll_interval_secs: 5,
            no_cache: false,
        };

        let error = build_compose_spec(&args, &inspection).unwrap_err();
        assert!(error.to_string().contains("包含多个服务: api, worker"));
    }
}
