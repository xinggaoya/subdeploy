use std::path::PathBuf;
use std::process;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use subdeploy_core::{default_remote_dir, deploy, ComposeSpec, DeployRequest, HealthCheckSpec};
use subdeploy_packager::{inspect_project, package_project, PackageRequest};
use subdeploy_remote::SshRemote;

#[derive(Debug, Parser)]
#[command(
    name = "subdeploy",
    version,
    about = "通过 SSH 部署 Docker 项目到远端服务器"
)]
struct Cli {
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

#[derive(Debug, Args)]
struct DeployArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// 远端主机
    #[arg(long)]
    host: String,
    /// SSH 端口
    #[arg(long, default_value_t = 22)]
    port: u16,
    /// SSH 用户名
    #[arg(long)]
    user: String,
    /// SSH 密码
    #[arg(long)]
    password: String,
    /// 容器服务名，用于读取日志
    #[arg(long)]
    service: String,
    /// 远端构建的镜像标签
    #[arg(long)]
    image_tag: String,
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
    let cli = Cli::parse();
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
    let remote_dir = args
        .remote_dir
        .unwrap_or_else(|| default_remote_dir(&inspection.project_name));

    let compose = ComposeSpec {
        dockerfile_rel: inspection.dockerfile_rel.clone(),
        compose_file_rel: inspection.compose_file_rel.clone(),
        service_name: args.service,
        image_tag: args.image_tag,
    };

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

impl From<ProjectArgs> for PackageRequest {
    fn from(value: ProjectArgs) -> Self {
        Self {
            project_dir: value.project_dir,
            dockerfile: value.dockerfile,
            compose_file: value.compose_file,
        }
    }
}
